import Foundation

/// The collapsed tool row's text: `Ran 5 commands, used a tool ›` (F1.3, spec 10 §4).
///
/// The phrase is derived from tool *names and counts*, not from a per-tool string table, so
/// a tool the harness gains tomorrow still reads as English ("used 2 tools") instead of
/// falling out of the sentence.
public struct ToolGroupSummary: Equatable, Sendable {
    public var phrase: String
    public var linesAdded: Int64
    public var linesRemoved: Int64
    public var isRunning: Bool
    public var hasError: Bool
    public var count: Int

    public var hasDiff: Bool { linesAdded > 0 || linesRemoved > 0 }

    public init(_ calls: [ToolCallDisplay]) {
        count = calls.count
        isRunning = calls.contains { $0.isRunning }
        hasError = calls.contains { $0.isError }
        linesAdded = calls.reduce(0) { $0 + ($1.linesAdded ?? 0) }
        linesRemoved = calls.reduce(0) { $0 + ($1.linesRemoved ?? 0) }
        phrase = Self.phrase(for: calls.map(\.name), running: isRunning)
    }

    // MARK: - Phrasing

    /// How a tool reads in the summary. `other` is the escape hatch every unknown name takes.
    enum Verb: Int, CaseIterable {
        case ran, read, created, edited, searched, fetched, other

        static func of(_ toolName: String) -> Verb {
            switch toolName.lowercased() {
            case "bash", "shell", "run", "exec": .ran
            case "read", "view", "cat": .read
            case "write", "create": .created
            case "edit", "patch", "apply_patch", "multiedit": .edited
            case "grep", "glob", "search", "find": .searched
            case "web_fetch", "webfetch", "fetch", "web_search": .fetched
            default: .other
            }
        }

        /// Present participle while the group is live, past tense once it is done.
        func clause(_ count: Int, running: Bool) -> String {
            switch self {
            case .ran: running ? "running \(count) \(plural(count, "command"))"
                : "ran \(count) \(plural(count, "command"))"
            case .read: running ? "reading \(count) \(plural(count, "file"))"
                : "read \(count) \(plural(count, "file"))"
            case .created: running ? "creating \(count) \(plural(count, "file"))"
                : "created \(count) \(plural(count, "file"))"
            case .edited: running ? "editing \(count) \(plural(count, "file"))"
                : "edited \(count) \(plural(count, "file"))"
            // "Searching" with no count is the reference's wording; a finished search says
            // how many it ran.
            case .searched: running ? "searching"
                : "searched \(count) \(plural(count, "pattern"))"
            case .fetched: running ? "fetching \(count) \(plural(count, "page"))"
                : "fetched \(count) \(plural(count, "page"))"
            case .other:
                if count == 1 { return running ? "using a tool" : "used a tool" }
                return running ? "using \(count) tools" : "used \(count) tools"
            }
        }

        private func plural(_ count: Int, _ noun: String) -> String {
            count == 1 ? noun : noun + "s"
        }
    }

    static func phrase(for names: [String], running: Bool) -> String {
        guard !names.isEmpty else { return running ? "Using a tool" : "Used a tool" }

        var counts: [Verb: Int] = [:]
        for name in names { counts[.of(name), default: 0] += 1 }

        // Most-used first, ties broken by the enum's order so the phrase is deterministic.
        let ordered = counts.sorted { a, b in
            a.value == b.value ? a.key.rawValue < b.key.rawValue : a.value > b.value
        }

        var clauses: [String] = []
        if ordered.count <= 2 {
            clauses = ordered.map { $0.key.clause($0.value, running: running) }
        } else {
            // Past two categories the sentence stops being a summary. Everything after the
            // leader collapses into the generic clause.
            clauses.append(ordered[0].key.clause(ordered[0].value, running: running))
            let rest = ordered.dropFirst().reduce(0) { $0 + $1.value }
            clauses.append(Verb.other.clause(rest, running: running))
        }

        return clauses.joined(separator: ", ").capitalizedFirst
    }
}

extension String {
    /// Sentence case only — `capitalized` would title-case every word in the phrase.
    var capitalizedFirst: String {
        guard let first else { return self }
        return first.uppercased() + dropFirst()
    }
}
