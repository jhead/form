import FormCore
import FormDesign
import SwiftUI

/// The content pane (spec 09 §4): Home, a session (44 pt header over the transcript), or the
/// empty state.
struct ContentShell<HomeContent: View, SessionContent: View>: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    let appState: AppState
    let workspace: WorkspaceRootController
    let home: () -> HomeContent
    let session: (String) -> SessionContent

    private var commands: SessionCommands {
        SessionCommands(stores: stores, appState: appState)
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentBackground()
            .overlay(alignment: .topLeading) { reopenSidebarButton }
    }

    /// The sidebar's own toggle lives *in* the sidebar, so collapsing it takes the control
    /// away with it and `⌘\` becomes the only way back — which is not discoverable and reads
    /// as a dead end. This is the way back in, and it appears only while there is one to
    /// need. It sits clear of the traffic lights, which move over this pane when the sidebar
    /// is gone.
    @ViewBuilder
    private var reopenSidebarButton: some View {
        if appState.sidebarCollapsed {
            IconButton(
                systemImage: "sidebar.leading",
                accessibilityLabel: "Show Sidebar"
            ) {
                appState.toggleSidebar()
            }
            .padding(.leading, theme.metrics.trafficLightInset)
            .padding(.top, theme.metrics.spacing.md)
            .formTooltip("Show Sidebar", detail: "⌘\\")
            .transition(.opacity)
        }
    }

    @ViewBuilder
    private var content: some View {
        switch appState.route {
        case .home:
            home()
        case .noSession:
            emptyState
        case let .session(id):
            if let summary = stores.sessions.session(id: id) {
                VStack(spacing: 0) {
                    SessionHeaderView(
                        session: summary, commands: commands, appState: appState,
                        workspace: workspace)
                    FormDivider()
                    session(id)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                // The route outlived the session — restoration from a previous launch, or a
                // delete that landed while it was open.
                emptyState
            }
        }
    }

    private var emptyState: some View {
        EmptyState(
            showsWordmark: true,
            title: "Nothing open",
            message: "Start a chat, or pick a session from the sidebar."
        ) {
            FormButton("New chat", systemImage: "plus", kind: .primary) {
                commands.newSession()
            }
        }
        .frame(maxWidth: theme.metrics.contentMaxWidth)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
