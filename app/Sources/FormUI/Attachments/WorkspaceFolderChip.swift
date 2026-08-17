import AppKit
import FormCore
import FormDesign
import SwiftUI

/// The composer's workspace-root affordance (F4.1–F4.5).
///
/// One chip showing the root's basename with the full path as a tooltip, and a picker menu
/// beside it: recent roots from `listRecentRoots`, a `Choose folder…` item opening an
/// `NSOpenPanel` in directory mode, and `Clear`. An unset root reads `Unconfined` in tertiary
/// rather than being silently absent (F4.5).
public struct WorkspaceFolderChip: View {
    @Environment(\.theme) private var theme

    private let controller: WorkspaceRootController

    @State private var isShowingMenu = false

    public init(controller: WorkspaceRootController) {
        self.controller = controller
    }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            if let root = controller.root {
                Chip(
                    URL(fileURLWithPath: root).lastPathComponent,
                    systemImage: "folder",
                    tooltip: root,
                    action: { isShowingMenu = true }
                )
            } else {
                Chip(
                    "Unconfined",
                    systemImage: "folder.badge.questionmark",
                    tooltip: "No workspace root — the agent is not confined to a folder",
                    action: { isShowingMenu = true }
                )
                // Tertiary, per F4.5: it is a state, not a warning.
                .foregroundStyle(theme.color.textTertiary)
            }

            Chip(systemImage: "folder.badge.plus", tooltip: "Choose folder…") {
                isShowingMenu = true
            }
            .popover(isPresented: $isShowingMenu, arrowEdge: .top) {
                menu
            }
        }
        .task { await controller.refreshRecents() }
    }

    private var menu: some View {
        PopoverContainer(title: "Workspace root") {
            ForEach(controller.recents) { workspace in
                Button {
                    isShowingMenu = false
                    controller.setRoot(workspace.path)
                } label: {
                    HStack(spacing: theme.metrics.spacing.md) {
                        Image(systemName: "folder")
                            .typeStyle(theme.typography.micro)
                            .foregroundStyle(theme.color.textTertiary)
                        Text(workspace.name)
                            .typeStyle(theme.typography.caption)
                            .foregroundStyle(theme.color.textPrimary)
                            .lineLimit(1)
                        Spacer(minLength: theme.metrics.spacing.md)
                        if workspace.path == controller.root {
                            Image(systemName: "checkmark")
                                .typeStyle(theme.typography.micro)
                                .foregroundStyle(theme.color.accent)
                        }
                    }
                    .frame(height: theme.metrics.sidebarRowHeight)
                    .contentShape(Rectangle())
                    .formTooltip(workspace.path)
                }
                .buttonStyle(.plain)
            }

            if !controller.recents.isEmpty { FormDivider() }

            FormButton("Choose folder…", systemImage: "folder", size: .small) {
                isShowingMenu = false
                controller.chooseFolder()
            }
            if controller.root != nil {
                FormButton("Clear", size: .small) {
                    isShowingMenu = false
                    controller.setRoot(nil)
                }
            }
        }
    }
}

/// The state and the three effects behind the chip, kept out of the view so `⌘⇧F` can run the
/// panel without one (W14's `chooseWorkspaceFolder` hook).
@MainActor
@Observable
public final class WorkspaceRootController {
    public private(set) var recents: [Workspace] = []

    @ObservationIgnored private let stores: CoreStores

    public init(stores: CoreStores) {
        self.stores = stores
    }

    public var sessionId: String? { stores.sessions.selectedSessionId }
    public var root: String? { stores.sessions.selected?.workspaceRoot }

    public func refreshRecents() async {
        recents = (try? await stores.client.query(ListRecentRoots())) ?? []
    }

    /// Directories only (F4.1).
    public func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Confine this session to a folder"
        if let root { panel.directoryURL = URL(fileURLWithPath: root) }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        setRoot(url.path)
    }

    /// The `⌘⇧F` seam W14 declared for this workstream (`CommandHooks.chooseWorkspaceFolder`).
    /// One line at startup: `center.hooks.chooseWorkspaceFolder =
    /// WorkspaceRootController.folderPickerHook(stores)`.
    public static func folderPickerHook(_ stores: CoreStores) -> @MainActor () -> Void {
        let controller = WorkspaceRootController(stores: stores)
        return { controller.chooseFolder() }
    }

    public func setRoot(_ path: String?) {
        guard let sessionId else { return }
        Task {
            _ = try? await stores.client.dispatch(
                .setWorkspaceRoot(sessionId: sessionId, path: path))
            // The core records the root as recently used; re-read rather than guess.
            await refreshRecents()
        }
    }
}

#Preview("Workspace folder chip") {
    WorkspaceFolderChipPreview()
}

private struct WorkspaceFolderChipPreview: View {
    @State private var controller = WorkspaceRootController(stores: .preview(.populated))

    var body: some View {
        ThemePreview {
            WorkspaceFolderChip(controller: controller)
        }
    }
}
