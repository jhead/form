import FormCore
import SwiftUI

/// One rendered section of the sidebar: a group and its rows, or the trailing `Ungrouped`
/// section (spec 09 §2).
public struct SidebarSection: Identifiable, Equatable {
    public let group: SessionGroup?
    public let sessions: [SessionSummary]

    public var id: String { group?.id ?? "ungrouped" }
    public var name: String { group?.name ?? "Ungrouped" }
    public var isCollapsed: Bool { group?.collapsed ?? false }
}

/// The sidebar's display order, and the flattened sequence `⌘1`–`⌘9` index into.
///
/// **Why not `SessionStore.ordered`.** That property sorts by pinned-then-`updatedAt`, which
/// discards the dense manual `index` the core maintains — so a session dragged to a new
/// position would snap back on the next event. The core already returns rows in
/// group-then-index order and renumbers densely on `moveSession`, and a freshly created
/// session lands at index 0, so ordering by `index` is *also* newest-first (F2.1) while
/// letting a drag stick (F2.3). See the W9 report.
public enum SidebarOrder {
    public static func sections(in store: SessionStore) -> [SidebarSection] {
        let groups = store.groups.sorted { $0.index < $1.index }
        var sections = groups.map { group in
            SidebarSection(group: group, sessions: sessions(in: group, store: store))
        }
        sections.append(SidebarSection(group: nil, sessions: sessions(in: nil, store: store)))
        return sections
    }

    public static func sessions(
        in group: SessionGroup?, store: SessionStore
    ) -> [SessionSummary] {
        store.sessions
            .filter { store.includeArchived || !$0.archived }
            .filter { $0.groupId == group?.id }
            .sorted { a, b in
                if a.pinned != b.pinned { return a.pinned }
                if a.index != b.index { return a.index < b.index }
                return a.updatedAt > b.updatedAt
            }
    }

    /// The rows actually on screen, top to bottom. A collapsed group contributes nothing —
    /// its rows are not there to be numbered, and `⌘3` must land on the third visible row.
    public static func visibleSessions(in store: SessionStore) -> [SessionSummary] {
        sections(in: store).flatMap { $0.isCollapsed ? [] : $0.sessions }
    }

    /// `⌘1`–`⌘9` (F2.1, spec 14 §2). W14 should resolve rank through this rather than
    /// `SessionStore.session(rank:)`, which uses the other ordering.
    public static func session(rank: Int, in store: SessionStore) -> SessionSummary? {
        let visible = visibleSessions(in: store)
        guard rank >= 1, rank <= visible.count else { return nil }
        return visible[rank - 1]
    }

    /// Rank per session id, for the rows that get a number. Only the first nine rows do.
    public static func ranks(in store: SessionStore) -> [String: Int] {
        var ranks: [String: Int] = [:]
        for (offset, session) in visibleSessions(in: store).prefix(9).enumerated() {
            ranks[session.id] = offset + 1
        }
        return ranks
    }
}
