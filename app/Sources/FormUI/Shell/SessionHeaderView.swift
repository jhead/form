import FormCore
import FormDesign
import SwiftUI

/// The 44 pt header above the transcript (spec 09 §4, spec 08 §1): editable title, workspace
/// chip, trailing icon buttons and a `⋮` overflow.
struct SessionHeaderView: View {
    @Environment(\.theme) private var theme

    let session: SessionSummary
    let commands: SessionCommands
    let appState: AppState
    /// W13 owns the picker, its recents list and the Clear action; the header only places it.
    let workspace: WorkspaceRootController

    @State private var isRenaming = false
    @State private var deleteTarget: SessionSummary?
    @State private var newGroupPresented = false
    @State private var newGroupName = ""

    var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            title
            WorkspaceFolderChip(controller: workspace)
            if session.status == .streaming {
                PulsingDot()
                    .accessibilityLabel("Streaming")
            }

            Spacer(minLength: theme.metrics.spacing.lg)

            IconButton(systemImage: "text.magnifyingglass", accessibilityLabel: "Find in Session") {
                appState.findPresented = true
            }
            IconButton(
                systemImage: session.pinned ? "pin.fill" : "pin",
                accessibilityLabel: session.pinned ? "Unpin Session" : "Pin Session",
                isActive: session.pinned
            ) {
                commands.setPinned(session, !session.pinned)
            }
            IconButton(systemImage: "archivebox", accessibilityLabel: "Archive Session") {
                commands.archive(session)
            }
            SidebarMenuButton(
                systemImage: "ellipsis",
                accessibilityLabel: "Session Actions",
                rotation: .degrees(90)
            ) {
                SessionMenuItems(
                    session: session,
                    commands: commands,
                    onRename: { isRenaming = true },
                    onNewGroup: {
                        newGroupName = ""
                        newGroupPresented = true
                    },
                    onDelete: requestDelete
                )
            }
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .frame(height: theme.metrics.headerHeight)
        .frame(maxWidth: .infinity)
        .alert("New Group", isPresented: $newGroupPresented) {
            TextField("Name", text: $newGroupName)
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                commands.createGroup(named: newGroupName, movingSession: session.id)
            }
        }
        .alert(
            "Delete Session",
            isPresented: Binding(
                get: { deleteTarget != nil }, set: { if !$0 { deleteTarget = nil } }),
            presenting: deleteTarget
        ) { target in
            Button("Cancel", role: .cancel) { deleteTarget = nil }
            Button("Delete", role: .destructive) {
                commands.delete(target.id)
                deleteTarget = nil
            }
        } message: { target in
            Text("\(target.title) and its transcript will be removed.")
        }
    }

    @ViewBuilder
    private var title: some View {
        if isRenaming {
            InlineEditField(
                initialText: session.title,
                style: theme.typography.body.weighted(.medium),
                accessibilityLabel: "Session title",
                onCommit: { title in
                    commands.rename(session.id, to: title)
                    isRenaming = false
                },
                onCancel: { isRenaming = false }
            )
            .frame(maxWidth: theme.metrics.contentMaxWidth / 2)
        } else {
            Text(session.title)
                .typeStyle(theme.typography.body.weighted(.medium))
                .foregroundStyle(theme.color.textPrimary)
                .lineLimit(1)
                .truncationMode(.tail)
                .contentShape(Rectangle())
                .onTapGesture(count: 2) { isRenaming = true }
                .accessibilityLabel("Session title")
                .accessibilityValue(session.title)
                .accessibilityHint("Double-click to rename")
        }
    }

    private func requestDelete() {
        if commands.confirmOnDelete {
            deleteTarget = session
        } else {
            commands.delete(session.id)
        }
    }
}
