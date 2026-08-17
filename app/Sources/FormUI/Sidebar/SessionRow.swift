import FormCore
import FormDesign
import SwiftUI

/// One session in the sidebar (spec 09 §3, spec 08 §1): 32 pt tall, a 16 pt leading slot
/// carrying either the rank number or a status dot, then a tail-truncating title.
struct SessionRow: View {
    @Environment(\.theme) private var theme

    let session: SessionSummary
    let rank: Int?
    let isSelected: Bool
    let commands: SessionCommands

    @Binding var renamingSessionId: String?
    let onRequestDelete: (SessionSummary) -> Void
    let onRequestNewGroup: (SessionSummary) -> Void

    private var isRenaming: Bool { renamingSessionId == session.id }

    var body: some View {
        ListRow(isSelected: isSelected, height: theme.metrics.sidebarRowHeight) { state in
            HStack(spacing: theme.metrics.spacing.md) {
                leadingSlot(state)
                    .frame(width: theme.metrics.spacing.xl)

                if isRenaming {
                    InlineEditField(
                        initialText: session.title,
                        style: theme.typography.ui,
                        accessibilityLabel: "Session title",
                        onCommit: { title in
                            commands.rename(session.id, to: title)
                            renamingSessionId = nil
                        },
                        onCancel: { renamingSessionId = nil }
                    )
                } else {
                    Text(session.title)
                        .typeStyle(theme.typography.ui)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .foregroundStyle(
                            isSelected ? theme.color.textPrimary : theme.color.textSecondary)

                    Spacer(minLength: 0)

                    if session.pinned {
                        Image(systemName: "pin.fill")
                            .imageScale(.small)
                            .foregroundStyle(theme.color.textTertiary)
                            .accessibilityHidden(true)
                    }
                }
            }
        }
        .contentShape(Rectangle())
        // `ListRow` is built without an action on purpose: its own press-tracking
        // `DragGesture` would fight `.onDrag`, and its tap would swallow the double-click.
        // Simultaneous gestures sit alongside it instead (F2.5).
        .simultaneousGesture(TapGesture().onEnded { commands.select(session.id) })
        .simultaneousGesture(TapGesture(count: 2).onEnded { beginRename() })
        .contextMenu {
            SessionMenuItems(
                session: session,
                commands: commands,
                onRename: beginRename,
                onNewGroup: { onRequestNewGroup(session) },
                onDelete: { onRequestDelete(session) }
            )
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(session.title)
        .accessibilityValue(accessibilityValue)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
        .accessibilityHint("Opens this session")
    }

    /// Rank number normally; a status dot on hover, or whenever the session is streaming or
    /// errored (spec 09 §3).
    @ViewBuilder
    private func leadingSlot(_ state: ListRowState) -> some View {
        if showsStatusDot(state) {
            PulsingDot(isActive: isStreaming, tone: dotTone)
        } else if let rank {
            Text("\(rank)")
                .typeStyle(theme.typography.micro)
                .tabularFigures()
                .foregroundStyle(theme.color.textTertiary)
        } else {
            // Past rank 9 an unhovered row has nothing in the slot, but the slot still has
            // to hold its width so the titles line up.
            theme.color.surface.opacity(0)
        }
    }

    private func showsStatusDot(_ state: ListRowState) -> Bool {
        isStreaming || isErrored || state.isHovering || state.isSelected
    }

    private var isStreaming: Bool { session.status == .streaming }
    private var isErrored: Bool { session.status == .error }

    private var dotTone: FormTone {
        if isErrored { return .danger }
        if isStreaming { return .accent }
        return .neutral
    }

    private var accessibilityValue: String {
        var parts: [String] = []
        if let rank { parts.append("Rank \(rank)") }
        parts.append(statusDescription)
        if session.pinned { parts.append("Pinned") }
        return parts.joined(separator: ", ")
    }

    private var statusDescription: String {
        switch session.status {
        case .streaming: "Streaming"
        case .error: "Error"
        case .idle: "Idle"
        default: session.status.rawValue
        }
    }

    private func beginRename() {
        renamingSessionId = session.id
    }
}

#Preview("SessionRow") {
    SessionRowPreview()
}

private struct SessionRowPreview: View {
    @State private var stores = CoreStores.preview(.populated)
    @State private var appState = AppState()
    @State private var renaming: String?

    var body: some View {
        let commands = SessionCommands(stores: stores, appState: appState)
        let sessions = Array(SidebarOrder.visibleSessions(in: stores.sessions).prefix(4))

        ThemePreview {
            VStack(spacing: 0) {
                ForEach(Array(sessions.enumerated()), id: \.element.id) { index, session in
                    SessionRow(
                        session: session,
                        rank: index + 1,
                        isSelected: index == 0,
                        commands: commands,
                        renamingSessionId: $renaming,
                        onRequestDelete: { _ in },
                        onRequestNewGroup: { _ in }
                    )
                }
            }
            .frame(width: 300)
        }
    }
}
