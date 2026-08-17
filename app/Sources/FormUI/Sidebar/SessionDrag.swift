import FormCore
import FormDesign
import SwiftUI

/// Where a dragged session would land: a group (or `Ungrouped`) and a row index within it.
struct SidebarDropTarget: Equatable {
    var groupId: String?
    var index: Int
}

/// Live drag state for the sidebar (F2.3).
///
/// The dragged session's id is held here rather than read back out of the `NSItemProvider`.
/// Reading the provider is asynchronous, and the insertion indicator has to track the pointer
/// at frame rate — an await per `dropUpdated` would make it lag the cursor. The provider is
/// still supplied so the drag reads correctly to AppKit; nothing needs to decode it, because
/// the only thing that can start one of these drags is a row in this sidebar.
@MainActor
@Observable
final class SessionDragState {
    private(set) var draggingSessionId: String?
    var target: SidebarDropTarget?

    func begin(_ sessionId: String) {
        draggingSessionId = sessionId
        target = nil
    }

    func end() {
        draggingSessionId = nil
        target = nil
    }

    var isDragging: Bool { draggingSessionId != nil }

    func isTargeting(groupId: String?, index: Int) -> Bool {
        target == SidebarDropTarget(groupId: groupId, index: index)
    }

    /// The item provider a row hands AppKit. Plain text, so no custom UTI has to be declared
    /// in `Info.plist` for the drag to be valid.
    nonisolated static func provider(for sessionId: String) -> NSItemProvider {
        NSItemProvider(object: sessionId as NSString)
    }
}

/// A drop landing between rows. Attached to each session row; the pointer's position within
/// the row decides whether the insertion is above or below it.
struct SessionRowDropDelegate: DropDelegate {
    let groupId: String?
    let rowIndex: Int
    let rowHeight: CGFloat
    let state: SessionDragState
    let commands: SessionCommands
    let sessions: [SessionSummary]

    func validateDrop(info: DropInfo) -> Bool {
        MainActor.assumeIsolated { state.isDragging }
    }

    func dropEntered(info: DropInfo) {
        MainActor.assumeIsolated { state.target = target(for: info) }
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        MainActor.assumeIsolated { state.target = target(for: info) }
        return DropProposal(operation: .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        MainActor.assumeIsolated {
            let resolved = target(for: info)
            defer { state.end() }
            return apply(resolved, state: state, commands: commands, sessions: sessions)
        }
    }

    private func target(for info: DropInfo) -> SidebarDropTarget {
        let insertsAbove = info.location.y < rowHeight / 2
        return SidebarDropTarget(groupId: groupId, index: insertsAbove ? rowIndex : rowIndex + 1)
    }
}

/// A drop on a group header or on an empty group's `Drag or move sessions here` row —
/// "put it in this group", with no position implied (spec 09 §2).
struct SessionSectionDropDelegate: DropDelegate {
    let groupId: String?
    let index: Int
    let state: SessionDragState
    let commands: SessionCommands
    let sessions: [SessionSummary]

    func validateDrop(info: DropInfo) -> Bool {
        MainActor.assumeIsolated { state.isDragging }
    }

    func dropEntered(info: DropInfo) {
        MainActor.assumeIsolated {
            state.target = SidebarDropTarget(groupId: groupId, index: index)
        }
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        DropProposal(operation: .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        MainActor.assumeIsolated {
            defer { state.end() }
            return apply(
                SidebarDropTarget(groupId: groupId, index: index),
                state: state, commands: commands, sessions: sessions
            )
        }
    }
}

/// Shared by both delegates: dispatch the move, correcting the index for the row's own
/// removal when it is moving down within the same section.
@MainActor
private func apply(
    _ target: SidebarDropTarget,
    state: SessionDragState,
    commands: SessionCommands,
    sessions: [SessionSummary]
) -> Bool {
    guard let sessionId = state.draggingSessionId else { return false }

    var index = target.index
    if let current = sessions.firstIndex(where: { $0.id == sessionId }), current < index {
        index -= 1
    }
    // A no-op move still round-trips through the core, which is pointless churn.
    if let current = sessions.firstIndex(where: { $0.id == sessionId }),
        current == index,
        sessions[current].groupId == target.groupId
    {
        return false
    }

    commands.move(sessionId, toGroup: target.groupId, index: max(0, index))
    return true
}

/// The 2 pt insertion line (spec 09 §3).
struct DropInsertionIndicator: View {
    @Environment(\.theme) private var theme

    var body: some View {
        Capsule(style: .continuous)
            .fill(theme.color.accent)
            .frame(height: theme.metrics.dropIndicator)
            .padding(.horizontal, theme.metrics.spacing.md)
            .accessibilityHidden(true)
    }
}
