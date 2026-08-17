import FormCore
import FormDesign
import SwiftUI

/// The sidebar (spec 09 §2, laid out to spec 08 §1).
///
/// Top to bottom: control row over the traffic lights, the `Home` / `Code` segment, the
/// `+ New` row, the scrolling group sections, and a pinned footer.
public struct SidebarView: View {
    @Environment(\.theme) private var theme

    private let stores: CoreStores
    private let appState: AppState
    private let themeController: ThemeController
    @Binding private var ungroupedCollapsed: Bool

    @State private var dragState = SessionDragState()
    @State private var renamingSessionId: String?
    @State private var groupRenameTarget: SessionGroup?
    @State private var groupRenameText = ""
    @State private var groupDeleteTarget: SessionGroup?
    @State private var sessionDeleteTarget: SessionSummary?
    @State private var newGroupPresented = false
    @State private var newGroupName = ""
    /// Set when "Move to ▸ New group…" opened the sheet, so the session follows the group.
    @State private var newGroupSessionId: String?

    public init(
        stores: CoreStores,
        appState: AppState,
        themeController: ThemeController,
        ungroupedCollapsed: Binding<Bool>
    ) {
        self.stores = stores
        self.appState = appState
        self.themeController = themeController
        _ungroupedCollapsed = ungroupedCollapsed
    }

    private var commands: SessionCommands {
        SessionCommands(stores: stores, appState: appState)
    }

    public var body: some View {
        withPrompts(
            VStack(spacing: 0) {
                controlRow
                segmentRow
                newRow
                list
                FormDivider(inset: theme.metrics.spacing.lg)
                SidebarFooter(
                    stores: stores, appState: appState, themeController: themeController)
            }
            .sidebarBackground()
        )
    }

    // MARK: - Row 1: controls over the traffic lights

    private var controlRow: some View {
        HStack(spacing: theme.metrics.spacing.xxs) {
            // The traffic lights sit here; nothing else may (spec 09 §1).
            Spacer(minLength: theme.metrics.trafficLightInset)

            IconButton(
                systemImage: "sidebar.leading",
                accessibilityLabel: "Toggle Sidebar",
                isActive: !appState.sidebarCollapsed
            ) {
                appState.toggleSidebar()
            }

            IconButton(systemImage: "magnifyingglass", accessibilityLabel: "Search Sessions") {
                appState.searchPresented = true
            }
        }
        .frame(height: theme.metrics.iconButton)
        .padding(.horizontal, theme.metrics.spacing.md)
        .padding(.top, theme.metrics.spacing.md)
    }

    // MARK: - Row 2: Home / Code

    private var segmentRow: some View {
        SegmentedToggle(
            selection: Binding(get: { appState.segment }, set: { appState.segment = $0 }),
            segments: SidebarSegment.allCases.map {
                .init(value: $0, title: $0.title, systemImage: $0.systemImage)
            }
        )
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.top, theme.metrics.spacing.md)
        .accessibilityLabel("Section")
    }

    // MARK: - Row 3: + New

    private var newRow: some View {
        ListRow(height: theme.metrics.navRowHeight, action: { commands.newSession() }) { _ in
            HStack(spacing: theme.metrics.spacing.md) {
                Image(systemName: "plus")
                    .typeStyle(theme.typography.uiMedium)
                    .foregroundStyle(theme.color.textSecondary)
                Text("New")
                    .typeStyle(theme.typography.ui)
                    .foregroundStyle(theme.color.textPrimary)
                Spacer(minLength: 0)
            }
        }
        .padding(.horizontal, theme.metrics.spacing.md)
        .padding(.vertical, theme.metrics.spacing.md)
        .accessibilityLabel("New Chat")
        .accessibilityAddTraits(.isButton)
    }

    // MARK: - Group sections

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
                if isEmpty {
                    EmptyState(
                        systemImage: "bubble.left.and.bubble.right",
                        title: "No sessions yet",
                        message: "Press New to start one.",
                        isCompact: true
                    )
                    .padding(.top, theme.metrics.spacing.xl)
                } else {
                    ForEach(sections) { section in
                        SidebarGroupSection(
                            section: section,
                            ranks: ranks,
                            selectedSessionId: appState.route.sessionId,
                            commands: commands,
                            dragState: dragState,
                            renamingSessionId: $renamingSessionId,
                            ungroupedCollapsed: $ungroupedCollapsed,
                            onRequestDelete: requestDelete,
                            onRequestNewGroup: { session in
                                newGroupSessionId = session.id
                                newGroupName = ""
                                newGroupPresented = true
                            },
                            onRenameGroup: { group in
                                groupRenameText = group.name
                                groupRenameTarget = group
                            },
                            onDeleteGroup: { group in
                                if commands.confirmOnDelete {
                                    groupDeleteTarget = group
                                } else {
                                    commands.deleteGroup(group.id)
                                }
                            }
                        )
                    }
                }
            }
            .padding(.horizontal, theme.metrics.spacing.md)
            .padding(.bottom, theme.metrics.spacing.lg)
        }
        .scrollContentBackground(.hidden)
        // A drag released outside every target still has to clear the indicator.
        .onChange(of: dragState.isDragging) { _, isDragging in
            if !isDragging { dragState.target = nil }
        }
    }

    private var sections: [SidebarSection] { SidebarOrder.sections(in: stores.sessions) }
    private var ranks: [String: Int] { SidebarOrder.ranks(in: stores.sessions) }

    private var isEmpty: Bool {
        stores.sessions.isLoaded && sections.allSatisfy { $0.sessions.isEmpty }
    }

    private func requestDelete(_ session: SessionSummary) {
        if commands.confirmOnDelete {
            sessionDeleteTarget = session
        } else {
            commands.delete(session.id)
        }
    }

    // MARK: - Confirmations and prompts
    //
    // Kept out of `body` so it stays about layout. All four are text-entry or destructive
    // paths that the row and header menus hand back here (spec 09 §3).
    private func withPrompts(_ content: some View) -> some View {
        content
            .alert(
                "Rename Group",
                isPresented: isPresented($groupRenameTarget),
                presenting: groupRenameTarget
            ) { group in
                TextField("Name", text: $groupRenameText)
                Button("Cancel", role: .cancel) { groupRenameTarget = nil }
                Button("Rename") {
                    commands.renameGroup(group.id, to: groupRenameText)
                    groupRenameTarget = nil
                }
            } message: { _ in
                Text("Choose a new name for this group.")
            }
            .alert("New Group", isPresented: $newGroupPresented) {
                TextField("Name", text: $newGroupName)
                Button("Cancel", role: .cancel) { newGroupSessionId = nil }
                Button("Create") {
                    commands.createGroup(named: newGroupName, movingSession: newGroupSessionId)
                    newGroupSessionId = nil
                }
            } message: {
                Text("Sessions can be dragged between groups afterwards.")
            }
            .alert(
                "Delete Session",
                isPresented: isPresented($sessionDeleteTarget),
                presenting: sessionDeleteTarget
            ) { session in
                Button("Cancel", role: .cancel) { sessionDeleteTarget = nil }
                Button("Delete", role: .destructive) {
                    commands.delete(session.id)
                    sessionDeleteTarget = nil
                }
            } message: { session in
                Text("\(session.title) and its transcript will be removed.")
            }
            .alert(
                "Delete Group",
                isPresented: isPresented($groupDeleteTarget),
                presenting: groupDeleteTarget
            ) { group in
                Button("Cancel", role: .cancel) { groupDeleteTarget = nil }
                Button("Delete", role: .destructive) {
                    commands.deleteGroup(group.id)
                    groupDeleteTarget = nil
                }
            } message: { _ in
                Text("Its sessions move to Ungrouped.")
            }
    }
}

/// `.alert(_:isPresented:presenting:)` wants a `Bool` binding alongside the value; this is
/// the one-liner that derives it from an optional.
@MainActor
private func isPresented<T>(_ binding: Binding<T?>) -> Binding<Bool> {
    Binding(
        get: { binding.wrappedValue != nil },
        set: { if !$0 { binding.wrappedValue = nil } }
    )
}

#Preview("Sidebar — populated") {
    SidebarPreview(scenario: .populated)
}

#Preview("Sidebar — empty") {
    SidebarPreview(scenario: .empty)
}

private struct SidebarPreview: View {
    let scenario: PreviewScenario

    @State private var stores: CoreStores
    @State private var appState = AppState()
    @State private var controller = ThemeController(mode: .light)
    @State private var ungroupedCollapsed = false

    init(scenario: PreviewScenario) {
        self.scenario = scenario
        _stores = State(initialValue: CoreStores.preview(scenario))
    }

    var body: some View {
        HStack(spacing: 0) {
            pane(.light)
            pane(.dark)
        }
        .frame(height: 700)
    }

    private func pane(_ theme: Theme) -> some View {
        SidebarView(
            stores: stores,
            appState: appState,
            themeController: controller,
            ungroupedCollapsed: $ungroupedCollapsed
        )
        .frame(width: theme.metrics.sidebarWidth)
        .theme(theme)
    }
}
