import Foundation

/// Groups, session summaries, selection and ordering (F2).
///
/// Every mutation goes to the core and comes back as an event; the only state this store
/// owns outright is `selectedSessionId` and the optimistic overlay a drag needs so the row
/// does not snap back for a frame.
@MainActor
@Observable
public final class SessionStore {
    public private(set) var groups: [SessionGroup] = []
    public private(set) var sessions: [SessionSummary] = []
    public private(set) var isLoaded = false
    public var includeArchived = false

    /// Selection lives here because the sidebar, the shortcut table and the command palette
    /// all drive it.
    public var selectedSessionId: String?

    @ObservationIgnored private let client: CoreClient

    public init(client: CoreClient) {
        self.client = client
    }

    /// For previews and tests: a store with no core behind it.
    public init(groups: [SessionGroup], sessions: [SessionSummary], client: CoreClient) {
        self.client = client
        self.groups = groups
        self.sessions = sessions
        selectedSessionId = sessions.first?.id
        isLoaded = true
    }

    /// Preview seeding — synchronous, so a `#Preview` renders populated on first pass
    /// instead of flashing an empty sidebar while a query resolves.
    func seed(_ corpus: MockCorpus) {
        groups = corpus.groups.sorted { $0.index < $1.index }
        sessions = corpus.sessions
        selectedSessionId = corpus.primarySessionId
        isLoaded = true
    }

    // MARK: - Loading

    public func load() async {
        do {
            let list = try await client.query(ListSessions(includeArchived: includeArchived))
            groups = list.groups.sorted { $0.index < $1.index }
            sessions = list.sessions
            isLoaded = true
            if selectedSessionId == nil { selectedSessionId = ordered.first?.id }
        } catch {
            Log.stores.error(
                "listSessions failed: \(String(describing: error), privacy: .public)")
        }
    }

    // MARK: - Events

    public func apply(_ event: CoreEvent) {
        switch event.kind {
        case let .sessionCreated(session):
            upsert(session)
            // A session the user just asked for should be the one they are looking at.
            if event.commandId != nil { selectedSessionId = session.id }
        case let .sessionUpdated(session):
            upsert(session)
        case let .sessionDeleted(sessionId):
            sessions.removeAll { $0.id == sessionId }
            if selectedSessionId == sessionId { selectedSessionId = ordered.first?.id }
        case let .groupsChanged(groups):
            self.groups = groups.sorted { $0.index < $1.index }
        default:
            break
        }
    }

    private func upsert(_ session: SessionSummary) {
        if let i = sessions.firstIndex(where: { $0.id == session.id }) {
            sessions[i] = session
        } else {
            sessions.append(session)
        }
    }

    // MARK: - Derived order

    /// Sidebar order: pinned first, then most recently updated (F2.1).
    public var ordered: [SessionSummary] {
        sessions
            .filter { includeArchived || !$0.archived }
            .sorted { a, b in
                if a.pinned != b.pinned { return a.pinned }
                return a.updatedAt > b.updatedAt
            }
    }

    public func sessions(in group: SessionGroup?) -> [SessionSummary] {
        ordered.filter { $0.groupId == group?.id }
    }

    /// Sessions with no group, rendered under `Ungrouped` (F2.2).
    public var ungrouped: [SessionSummary] { sessions(in: nil) }

    public var selected: SessionSummary? {
        selectedSessionId.flatMap { id in sessions.first { $0.id == id } }
    }

    public func session(id: String) -> SessionSummary? { sessions.first { $0.id == id } }

    /// `⌘1`–`⌘9` map onto the first nine rows (F2.1).
    public func session(rank: Int) -> SessionSummary? {
        let all = ordered
        guard rank >= 1, rank <= all.count else { return nil }
        return all[rank - 1]
    }

    public func selectNext(offset: Int = 1) {
        let all = ordered
        guard !all.isEmpty else { return }
        let current = all.firstIndex { $0.id == selectedSessionId } ?? 0
        let next = (current + offset + all.count) % all.count
        selectedSessionId = all[next].id
    }

    // MARK: - Commands

    @discardableResult
    public func createSession(
        groupId: String? = nil, title: String? = nil, workspaceRoot: String? = nil,
        modelRef: ModelRef? = nil
    ) async throws -> CommandID {
        try await client.dispatch(
            .createSession(
                groupId: groupId, title: title, workspaceRoot: workspaceRoot, modelRef: modelRef))
    }

    public func rename(_ sessionId: String, to title: String) async throws {
        // Echo locally so the inline editor commits without a round-trip flicker (F2.5).
        if let i = sessions.firstIndex(where: { $0.id == sessionId }) {
            sessions[i].title = title
            sessions[i].titleIsCustom = true
        }
        try await client.dispatch(.renameSession(sessionId: sessionId, title: title))
    }

    public func archive(_ sessionId: String, _ archived: Bool = true) async throws {
        try await client.dispatch(.archiveSession(sessionId: sessionId, archived: archived))
    }

    public func delete(_ sessionId: String) async throws {
        try await client.dispatch(.deleteSession(sessionId: sessionId))
    }

    public func pin(_ sessionId: String, _ pinned: Bool) async throws {
        try await client.dispatch(.pinSession(sessionId: sessionId, pinned: pinned))
    }

    /// Optimistic: the row moves now, and the next `session_updated`/`groups_changed`
    /// reconciles it (F2.3). If the core rejects the move, the event puts it back.
    public func move(_ sessionId: String, toGroup groupId: String?, index: Int) async throws {
        if let i = sessions.firstIndex(where: { $0.id == sessionId }) {
            sessions[i].groupId = groupId
            sessions[i].index = index
        }
        try await client.dispatch(
            .moveSession(sessionId: sessionId, groupId: groupId, index: index))
    }

    public func setModel(_ sessionId: String, _ modelRef: ModelRef) async throws {
        if let i = sessions.firstIndex(where: { $0.id == sessionId }) {
            sessions[i].modelRef = modelRef
        }
        try await client.dispatch(.setSessionModel(sessionId: sessionId, modelRef: modelRef))
    }

    public func setWorkspaceRoot(_ sessionId: String, path: String?) async throws {
        try await client.dispatch(.setWorkspaceRoot(sessionId: sessionId, path: path))
    }

    @discardableResult
    public func createGroup(name: String) async throws -> CommandID {
        try await client.dispatch(.createGroup(name: name))
    }

    public func renameGroup(_ groupId: String, to name: String) async throws {
        if let i = groups.firstIndex(where: { $0.id == groupId }) { groups[i].name = name }
        try await client.dispatch(.renameGroup(groupId: groupId, name: name))
    }

    public func deleteGroup(_ groupId: String) async throws {
        try await client.dispatch(.deleteGroup(groupId: groupId))
    }

    public func reorderGroup(_ groupId: String, to index: Int) async throws {
        try await client.dispatch(.reorderGroup(groupId: groupId, index: index))
    }

    /// Collapse state is echoed locally first: a disclosure triangle that waits for a round
    /// trip feels broken (F2.2).
    public func setCollapsed(_ groupId: String, _ collapsed: Bool) async throws {
        if let i = groups.firstIndex(where: { $0.id == groupId }) {
            groups[i].collapsed = collapsed
        }
        try await client.dispatch(.setGroupCollapsed(groupId: groupId, collapsed: collapsed))
    }

    // MARK: - Search

    public func search(_ q: String, limit: Int = 30) async -> [SearchHit] {
        guard !q.trimmingCharacters(in: .whitespaces).isEmpty else { return [] }
        do {
            return try await client.query(SearchSessions(q: q, limit: limit))
        } catch {
            Log.stores.error(
                "searchSessions failed: \(String(describing: error), privacy: .public)")
            return []
        }
    }
}
