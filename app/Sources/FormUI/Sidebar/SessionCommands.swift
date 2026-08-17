import FormCore
import FormDesign
import SwiftUI

/// Every session mutation the sidebar and the content header can perform, in one place so the
/// row's context menu and the header's `⋮` overflow cannot drift apart (spec 09 §3, §4).
///
/// The store methods are `async throws`; nothing here can usefully recover from a dispatch
/// failure, so each is fired into a task and logged. The outcome arrives as an event either
/// way — that is the protocol's contract (spec 00 §4).
@MainActor
struct SessionCommands {
    let stores: CoreStores
    let appState: AppState

    var confirmOnDelete: Bool { stores.settings.settings.general.confirmOnDelete }

    var groups: [SessionGroup] { stores.sessions.groups }

    func select(_ sessionId: String) {
        appState.navigate(to: .session(sessionId))
    }

    func newSession(in groupId: String? = nil) {
        run("createSession") { try await stores.newSession(groupId: groupId) }
    }

    func rename(_ sessionId: String, to title: String) {
        run("renameSession") { try await stores.sessions.rename(sessionId, to: title) }
    }

    /// The protocol has no `duplicateSession`, so this creates a fresh session carrying the
    /// original's title, group, model and workspace root. The transcript is not copied — see
    /// the W9 report.
    func duplicate(_ session: SessionSummary) {
        run("duplicateSession") {
            try await stores.sessions.createSession(
                groupId: session.groupId,
                title: "\(session.title) copy",
                workspaceRoot: session.workspaceRoot,
                modelRef: session.modelRef
            )
        }
    }

    func move(_ sessionId: String, toGroup groupId: String?, index: Int) {
        run("moveSession") {
            try await stores.sessions.move(sessionId, toGroup: groupId, index: index)
        }
    }

    func setPinned(_ session: SessionSummary, _ pinned: Bool) {
        run("pinSession") { try await stores.sessions.pin(session.id, pinned) }
    }

    func archive(_ session: SessionSummary) {
        run("archiveSession") {
            try await stores.sessions.archive(session.id, !session.archived)
        }
    }

    func delete(_ sessionId: String) {
        appState.forget(sessionId: sessionId)
        run("deleteSession") { try await stores.sessions.delete(sessionId) }
    }

    // MARK: Groups

    func createGroup(named name: String, movingSession sessionId: String? = nil) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        run("createGroup") {
            try await stores.sessions.createGroup(name: trimmed)
            // `groups_changed` lands before the ack resolves in practice, but the id is only
            // knowable by name — the core assigns it.
            guard let sessionId,
                let created = stores.sessions.groups.first(where: { $0.name == trimmed })
            else { return }
            try await stores.sessions.move(sessionId, toGroup: created.id, index: 0)
        }
    }

    func renameGroup(_ groupId: String, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        run("renameGroup") { try await stores.sessions.renameGroup(groupId, to: trimmed) }
    }

    func deleteGroup(_ groupId: String) {
        run("deleteGroup") { try await stores.sessions.deleteGroup(groupId) }
    }

    func setCollapsed(_ groupId: String, _ collapsed: Bool) {
        run("setGroupCollapsed") { try await stores.sessions.setCollapsed(groupId, collapsed) }
    }

    private func run(_ label: String, _ body: @escaping () async throws -> Void) {
        Task {
            do {
                try await body()
            } catch {
                Log.ui.error(
                    "\(label, privacy: .public) failed: \(String(describing: error), privacy: .public)"
                )
            }
        }
    }
}

/// The shared context/overflow menu for a session (spec 09 §3). The destructive and
/// text-entry paths are handed back to the host view, which owns the alert presentation.
struct SessionMenuItems: View {
    let session: SessionSummary
    let commands: SessionCommands
    let onRename: () -> Void
    let onNewGroup: () -> Void
    let onDelete: () -> Void

    var body: some View {
        Button("Rename", action: onRename)
        Button("Duplicate") { commands.duplicate(session) }

        Menu("Move to") {
            Button("Ungrouped") { commands.move(session.id, toGroup: nil, index: 0) }
                .disabled(session.groupId == nil)
            if !commands.groups.isEmpty { Divider() }
            ForEach(commands.groups) { group in
                Button(group.name) { commands.move(session.id, toGroup: group.id, index: 0) }
                    .disabled(session.groupId == group.id)
            }
            Divider()
            Button("New group…", action: onNewGroup)
        }

        Divider()

        Button(session.pinned ? "Unpin" : "Pin") {
            commands.setPinned(session, !session.pinned)
        }
        Button(session.archived ? "Unarchive" : "Archive") { commands.archive(session) }

        Divider()

        Button("Delete", role: .destructive, action: onDelete)
    }
}
