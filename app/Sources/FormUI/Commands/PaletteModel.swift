import FormCore
import Foundation
import Observation

/// A session row in the palette. Built either from a `searchSessions` hit — in which case
/// `ranges` are the core's — or from a recent session when the query is empty.
public struct PaletteSessionItem: Identifiable, Sendable, Equatable {
    public let sessionId: String
    public let entryId: String?
    public let title: String
    public let groupName: String?
    public let snippet: String
    public let ranges: [HighlightRange]
    public let timestamp: TimestampMs

    public var id: String { "\(sessionId)#\(entryId ?? "-")" }

    init(hit: SearchHit, groupName: String?) {
        sessionId = hit.sessionId
        entryId = hit.entryId
        title = hit.title
        self.groupName = groupName
        snippet = hit.snippet
        ranges = hit.highlights
        timestamp = hit.timestamp
    }

    init(session: SessionSummary, groupName: String?) {
        sessionId = session.id
        entryId = nil
        title = session.title
        self.groupName = groupName
        snippet = ""
        ranges = []
        timestamp = session.updatedAt
    }
}

/// A command row, with the ranges of the fuzzy match so the title can show what matched.
public struct PaletteCommandItem: Identifiable, Sendable {
    public let command: AppCommand
    public let ranges: [HighlightRange]
    public let score: Double

    public var id: String { command.id }
}

public struct PaletteGroupItem: Identifiable, Sendable, Equatable {
    public let group: SessionGroup
    public let ranges: [HighlightRange]
    public let sessionCount: Int

    public var id: String { group.id }
}

/// One selectable line, in the order `↑`/`↓` walk them.
public enum PaletteRow: Identifiable, Sendable {
    case session(PaletteSessionItem)
    case command(PaletteCommandItem)
    case group(PaletteGroupItem)

    public var id: String {
        switch self {
        case let .session(item): "session:\(item.id)"
        case let .command(item): "command:\(item.id)"
        case let .group(item): "group:\(item.id)"
        }
    }
}

/// `⌘K` — the command palette (F13.1, spec 14 §3).
@MainActor
@Observable
public final class PaletteModel {
    public var query = "" {
        didSet { if query != oldValue { scheduleSearch() } }
    }

    public private(set) var sessions: [PaletteSessionItem] = []
    public private(set) var commands: [PaletteCommandItem] = []
    public private(set) var groups: [PaletteGroupItem] = []
    public private(set) var isSearching = false
    /// Index into `rows`. The first result is preselected (spec 14 §3).
    public var selection = 0

    public static let sessionLimit = 6
    public static let commandLimit = 6
    public static let groupLimit = 4
    /// Queries are debounced and cancelled on change (spec 14 §3).
    static let debounce = Duration.milliseconds(120)

    private unowned let center: CommandCenter
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var generation = 0

    init(center: CommandCenter) {
        self.center = center
    }

    // MARK: - Lifecycle

    /// Called when `⌘K` opens the panel: show recents immediately rather than an empty box.
    public func begin() {
        query = ""
        loadEmptyState()
    }

    public func reset() {
        searchTask?.cancel()
        searchTask = nil
        query = ""
        sessions = []
        commands = []
        groups = []
        selection = 0
        isSearching = false
    }

    // MARK: - Rows

    public var rows: [PaletteRow] {
        sessions.map(PaletteRow.session)
            + commands.map(PaletteRow.command)
            + groups.map(PaletteRow.group)
    }

    public var isEmpty: Bool { rows.isEmpty }

    public var selectedRow: PaletteRow? {
        rows.indices.contains(selection) ? rows[selection] : nil
    }

    public func moveSelection(by delta: Int) {
        let count = rows.count
        guard count > 0 else { return }
        selection = ((selection + delta) % count + count) % count
    }

    public func select(_ row: PaletteRow) {
        if let index = rows.firstIndex(where: { $0.id == row.id }) { selection = index }
    }

    // MARK: - Activation

    /// `⏎`.
    public func activate(_ row: PaletteRow? = nil) async {
        guard let row = row ?? selectedRow else { return }
        center.dismiss(.palette)
        switch row {
        case let .session(item):
            await center.open(sessionId: item.sessionId)
        case let .command(item):
            await center.run(item.command)
        case let .group(item):
            // A group is not a route; opening one means going to its most recent session.
            if let first = center.stores.sessions.sessions(in: item.group).first {
                await center.open(sessionId: first.id)
            }
        }
    }

    /// `⌘⏎` — "open in a new session where meaningful" (spec 14 §3): a session or group row
    /// starts a fresh session in the same group. For a command there is no second meaning,
    /// so it behaves like `⏎`.
    public func activateInNewSession(_ row: PaletteRow? = nil) async {
        guard let row = row ?? selectedRow else { return }
        switch row {
        case let .session(item):
            center.dismiss(.palette)
            let groupId = center.stores.sessions.session(id: item.sessionId)?.groupId
            _ = try? await center.stores.newSession(groupId: groupId)
        case let .group(item):
            center.dismiss(.palette)
            _ = try? await center.stores.newSession(groupId: item.group.id)
        case .command:
            await activate(row)
        }
    }

    // MARK: - Searching

    private func scheduleSearch() {
        searchTask?.cancel()
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            isSearching = false
            loadEmptyState()
            return
        }

        // Commands are matched locally, so they can land on the very next frame while the
        // session query is still in flight — the panel never looks dead.
        commands = Self.match(commands: AppCommands.all, query: trimmed, center: center)
        groups = matchGroups(trimmed)
        selection = 0

        generation += 1
        let generation = generation
        isSearching = true
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: PaletteModel.debounce)
            if Task.isCancelled { return }
            guard let self else { return }
            let hits = await center.stores.sessions.search(trimmed, limit: Self.sessionLimit * 3)
            if Task.isCancelled { return }
            // Generation guard: results never arrive out of order (spec 14 §3).
            guard generation == self.generation else { return }
            self.adopt(hits: hits)
        }
    }

    private func adopt(hits: [SearchHit]) {
        let store = center.stores.sessions
        var seen = Set<String>()
        sessions = hits
            .sorted { $0.score > $1.score }
            .filter { seen.insert($0.id).inserted }
            .prefix(Self.sessionLimit)
            .map { hit in
                PaletteSessionItem(hit: hit, groupName: Self.groupName(for: hit.sessionId, in: store))
            }
        isSearching = false
        selection = min(selection, max(0, rows.count - 1))
    }

    /// Empty query: recent sessions and the most-used commands (spec 14 §3).
    private func loadEmptyState() {
        let store = center.stores.sessions
        sessions = store.ordered.prefix(Self.sessionLimit).map { session in
            PaletteSessionItem(
                session: session, groupName: Self.groupName(for: session.id, in: store))
        }
        commands = center.suggestedCommands(limit: Self.commandLimit).map {
            PaletteCommandItem(command: $0, ranges: [], score: 0)
        }
        groups = []
        selection = 0
    }

    private static func groupName(for sessionId: String, in store: SessionStore) -> String? {
        guard let groupId = store.session(id: sessionId)?.groupId else { return nil }
        return store.groups.first { $0.id == groupId }?.name
    }

    private func matchGroups(_ query: String) -> [PaletteGroupItem] {
        let store = center.stores.sessions
        return store.groups
            .compactMap { group -> PaletteGroupItem? in
                guard let ranges = FuzzyMatch.ranges(of: query, in: group.name), !ranges.isEmpty
                else { return nil }
                return PaletteGroupItem(
                    group: group, ranges: ranges, sessionCount: store.sessions(in: group).count)
            }
            .prefix(Self.groupLimit)
            .map { $0 }
    }

    static func match(
        commands: [AppCommand], query: String, center: CommandCenter?
    ) -> [PaletteCommandItem] {
        commands
            .compactMap { command -> PaletteCommandItem? in
                guard let score = FuzzyMatch.score(query, in: command.title, keywords: command.keywords)
                else { return nil }
                let ranges = FuzzyMatch.ranges(of: query, in: command.title) ?? []
                // A recently used command should sort above an equally good stranger.
                let usage = Double(center?.usageCount(command.id) ?? 0)
                return PaletteCommandItem(
                    command: command, ranges: ranges, score: score + min(usage, 5) * 0.02)
            }
            .sorted { $0.score > $1.score }
            .prefix(commandLimit)
            .map { $0 }
    }
}
