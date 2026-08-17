import AppKit
import SwiftUI
import FormCore
import FormDesign

/// The chips above the field: scope, workspace folder, and the folder picker (spec 10 §6,
/// F4.1–F4.5).
struct ComposerChipRow: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores

    @State private var recents: [Workspace] = []
    @State private var isShowingRecents = false

    private var session: SessionSummary? { stores.sessions.selected }

    var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            Chip("Local", systemImage: "laptopcomputer")

            if let root = session?.workspaceRoot {
                // Basename on the chip, full path on hover (F4.2).
                Chip(
                    URL(fileURLWithPath: root).lastPathComponent,
                    systemImage: "folder",
                    tooltip: root,
                    action: chooseFolder)
            } else {
                // A session with no root is explicitly unconfined, not silently rootless
                // (F4.5).
                Chip(
                    "Unconfined", systemImage: "folder.badge.questionmark", tone: .warning,
                    tooltip: "No workspace root — the agent is not confined to a folder",
                    action: chooseFolder)
            }

            Chip(
                systemImage: "folder.badge.plus", tooltip: "Choose folder…"
            ) {
                isShowingRecents = true
            }
            .popover(isPresented: $isShowingRecents, arrowEdge: .top) {
                PopoverContainer(title: "Workspace root") {
                    // Recently used roots are offered before the panel (F4.4).
                    ForEach(recents) { workspace in
                        Button {
                            setRoot(workspace.path)
                            isShowingRecents = false
                        } label: {
                            HStack(spacing: theme.metrics.spacing.md) {
                                Image(systemName: "folder")
                                    .typeStyle(theme.typography.micro)
                                    .foregroundStyle(theme.color.textTertiary)
                                Text(workspace.name)
                                    .typeStyle(theme.typography.caption)
                                    .foregroundStyle(theme.color.textPrimary)
                                Spacer(minLength: theme.metrics.spacing.md)
                            }
                            .frame(height: theme.metrics.sidebarRowHeight)
                            .contentShape(Rectangle())
                            .formTooltip(workspace.path)
                        }
                        .buttonStyle(.plain)
                    }
                    if !recents.isEmpty { FormDivider() }
                    FormButton("Choose folder…", systemImage: "folder", size: .small) {
                        isShowingRecents = false
                        chooseFolder()
                    }
                    if session?.workspaceRoot != nil {
                        FormButton("Clear root", size: .small) {
                            isShowingRecents = false
                            setRoot(nil)
                        }
                    }
                }
            }
            .task { recents = (try? await stores.client.query(ListRecentRoots())) ?? [] }

            Spacer(minLength: 0)
        }
    }

    /// Directories only, per spec 10 §6. `NSOpenPanel` is AppKit's, so this is one of the
    /// few places the view layer talks to it directly.
    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Confine this session to a folder"
        if let root = session?.workspaceRoot {
            panel.directoryURL = URL(fileURLWithPath: root)
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        setRoot(url.path)
    }

    private func setRoot(_ path: String?) {
        guard let sessionId = session?.id else { return }
        Task {
            try? await stores.sessions.setWorkspaceRoot(sessionId, path: path)
            recents = (try? await stores.client.query(ListRecentRoots())) ?? []
        }
    }
}
