import FormCore
import FormDesign
import SwiftUI
import UniformTypeIdentifiers

/// One group section: header, rows, and the drop targets between and around them
/// (spec 09 §2, §3).
struct SidebarGroupSection: View {
    @Environment(\.theme) private var theme

    let section: SidebarSection
    let ranks: [String: Int]
    let selectedSessionId: String?
    let commands: SessionCommands
    let dragState: SessionDragState

    @Binding var renamingSessionId: String?
    @Binding var ungroupedCollapsed: Bool

    let onRequestDelete: (SessionSummary) -> Void
    let onRequestNewGroup: (SessionSummary) -> Void
    let onRenameGroup: (SessionGroup) -> Void
    let onDeleteGroup: (SessionGroup) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if isExpanded {
                if section.sessions.isEmpty {
                    emptyDropTarget
                } else {
                    rows
                }
            }
        }
        .animation(theme.motion.animation(.fast), value: isExpanded)
        .onDrop(
            of: [UTType.text],
            delegate: SessionSectionDropDelegate(
                groupId: section.group?.id,
                index: section.sessions.count,
                state: dragState,
                commands: commands,
                sessions: section.sessions
            )
        )
    }

    // MARK: Header

    private var header: some View {
        SectionHeader(
            section.name,
            subtitle: section.sessions.isEmpty ? nil : "\(section.sessions.count)",
            isExpanded: expansionBinding
        ) { isHovering in
            if let group = section.group {
                SidebarMenuButton(systemImage: "ellipsis", accessibilityLabel: "Group actions") {
                    Button("Rename Group") { onRenameGroup(group) }
                    Button("New Session in Group") { commands.newSession(in: group.id) }
                    Divider()
                    Button("Delete Group", role: .destructive) { onDeleteGroup(group) }
                }
                .opacity(isHovering ? 1 : 0)
            }
        }
        .padding(.horizontal, theme.metrics.spacing.lg)
    }

    /// The core owns a real group's collapse state; `Ungrouped` is not a row in the core, so
    /// its disclosure is shell state persisted alongside the other window preferences.
    private var expansionBinding: Binding<Bool> {
        if let group = section.group {
            Binding(
                get: { !group.collapsed },
                set: { commands.setCollapsed(group.id, !$0) }
            )
        } else {
            Binding(
                get: { !ungroupedCollapsed },
                set: { ungroupedCollapsed = !$0 }
            )
        }
    }

    private var isExpanded: Bool {
        section.group.map { !$0.collapsed } ?? !ungroupedCollapsed
    }

    // MARK: Rows

    private var rows: some View {
        ForEach(Array(section.sessions.enumerated()), id: \.element.id) { index, session in
            SessionRow(
                session: session,
                rank: ranks[session.id],
                isSelected: session.id == selectedSessionId,
                commands: commands,
                renamingSessionId: $renamingSessionId,
                onRequestDelete: onRequestDelete,
                onRequestNewGroup: onRequestNewGroup
            )
            .opacity(dragState.draggingSessionId == session.id ? 0.4 : 1)
            .overlay(alignment: .top) {
                indicator(at: index)
            }
            .overlay(alignment: .bottom) {
                if index == section.sessions.count - 1 { indicator(at: index + 1) }
            }
            .onDrag {
                dragState.begin(session.id)
                return SessionDragState.provider(for: session.id)
            }
            .onDrop(
                of: [UTType.text],
                delegate: SessionRowDropDelegate(
                    groupId: section.group?.id,
                    rowIndex: index,
                    rowHeight: theme.metrics.sidebarRowHeight,
                    state: dragState,
                    commands: commands,
                    sessions: section.sessions
                )
            )
        }
    }

    @ViewBuilder
    private func indicator(at index: Int) -> some View {
        if dragState.isTargeting(groupId: section.group?.id, index: index) {
            DropInsertionIndicator()
        }
    }

    // MARK: Empty group

    private var emptyDropTarget: some View {
        Text("Drag or move sessions here")
            .typeStyle(theme.typography.micro)
            .italic()
            .foregroundStyle(theme.color.textTertiary)
            .padding(.horizontal, theme.metrics.spacing.lg + theme.metrics.spacing.xl)
            .frame(height: theme.metrics.emptyGroupRowHeight)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                    .strokeBorder(
                        isTargetedWhileEmpty ? theme.color.accent : theme.color.border,
                        style: StrokeStyle(
                            lineWidth: theme.metrics.hairline * 2,
                            dash: [theme.metrics.spacing.xs, theme.metrics.spacing.xs])
                    )
            )
            .animation(theme.motion.animation(.fast), value: isTargetedWhileEmpty)
    }

    private var isTargetedWhileEmpty: Bool {
        dragState.isTargeting(groupId: section.group?.id, index: 0)
    }
}
