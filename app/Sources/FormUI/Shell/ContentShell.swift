import FormCore
import FormDesign
import SwiftUI

/// The content pane (spec 09 §4): Home, a session (44 pt header over the transcript), or the
/// empty state.
struct ContentShell<HomeContent: View, SessionContent: View>: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    let appState: AppState
    let home: () -> HomeContent
    let session: (String) -> SessionContent

    private var commands: SessionCommands {
        SessionCommands(stores: stores, appState: appState)
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentBackground()
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
                    SessionHeaderView(session: summary, commands: commands, appState: appState)
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
