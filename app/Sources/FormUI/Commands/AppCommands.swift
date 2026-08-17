import AppKit
import FormCore
import FormDesign
import SwiftUI

/// **The single shortcut table** (F12.3, spec 14 §1).
///
/// Every key equivalent in `form` is declared here and nowhere else. Adding a shortcut means
/// adding a row; the menu bar, the key monitor, the command palette, the cheat sheet and the
/// Shortcuts preferences tab pick it up with no other edit. `AppCommandTableTests` fails the
/// build if a key equivalent is declared anywhere but here.
public enum AppCommands {
    public static let all: [AppCommand] = file + edit + view + session + navigate + help

    public static func command(id: String) -> AppCommand? {
        byID[id]
    }

    private static let byID: [String: AppCommand] = Dictionary(
        all.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })

    public static func commands(in category: CommandCategory) -> [AppCommand] {
        all.filter { $0.category == category }
    }

    // MARK: - File

    static let file: [AppCommand] = [
        AppCommand(
            id: "session.new",
            title: "New Chat",
            category: .file,
            defaultKey: KeyBinding("n", .command),
            systemImage: "square.and.pencil",
            keywords: ["create", "start", "conversation"]
        ) { context in
            _ = try? await context.stores.newSession()
        },

        AppCommand(
            id: "session.newInGroup",
            title: "New Chat in Current Group",
            category: .file,
            defaultKey: KeyBinding("n", [.command, .shift]),
            systemImage: "folder.badge.plus",
            keywords: ["create", "group"],
            isEnabled: { $0.activeSession?.groupId != nil }
        ) { context in
            _ = try? await context.stores.newSession(groupId: context.activeSession?.groupId)
        },

        AppCommand(
            id: "session.newFromCurrent",
            title: "New from Current",
            category: .file,
            defaultKey: KeyBinding("k", [.command, .shift]),
            systemImage: "arrow.triangle.branch",
            keywords: ["clear", "reset", "fork", "duplicate"],
            isEnabled: { $0.activeSession != nil }
        ) { context in
            guard let session = context.activeSession else { return }
            // "Clear" means a fresh transcript that keeps the working context — same group,
            // same workspace, same model — not a wiped session.
            _ = try? await context.stores.sessions.createSession(
                groupId: session.groupId,
                workspaceRoot: session.workspaceRoot,
                modelRef: session.modelRef)
        },

        AppCommand(
            id: "session.archive",
            title: "Close Session",
            category: .file,
            defaultKey: KeyBinding("w", .command),
            systemImage: "archivebox",
            keywords: ["archive", "close", "hide"],
            isEnabled: { $0.activeSessionId != nil }
        ) { context in
            // Single-window app: ⌘W archives rather than closing the window (spec 14 §2).
            guard let id = context.activeSessionId else { return }
            try? await context.stores.sessions.archive(id)
        },

        AppCommand(
            id: "workspace.choose",
            title: "Choose Workspace Folder…",
            category: .file,
            defaultKey: KeyBinding("f", [.command, .shift]),
            systemImage: "folder",
            keywords: ["root", "directory", "project"]
        ) { context in
            context.hooks.chooseWorkspaceFolder?()
        },

        AppCommand(
            id: "app.preferences",
            title: "Preferences…",
            category: .file,
            defaultKey: KeyBinding(",", .command),
            systemImage: "gearshape",
            keywords: ["settings", "options", "configure"]
        ) { context in
            context.hooks.openPreferences?(nil)
        },

        AppCommand(
            id: "window.close",
            title: "Close Window",
            category: .file,
            defaultKey: KeyBinding("w", [.command, .shift]),
            systemImage: "xmark.rectangle",
            keywords: ["quit window", "hide"]
        ) { _ in
            NSApp?.keyWindow?.performClose(nil)
        },
    ]

    // MARK: - Edit

    static let edit: [AppCommand] = [
        AppCommand(
            id: "find.open",
            title: "Find in Session…",
            category: .edit,
            defaultKey: KeyBinding("f", .command),
            systemImage: "text.magnifyingglass",
            keywords: ["search", "highlight", "match"],
            isEnabled: { $0.activeSessionId != nil }
        ) { context in
            context.center.openFind(seed: context.hooks.selectedText?())
        },

        AppCommand(
            id: "find.next",
            title: "Find Next",
            category: .edit,
            defaultKey: KeyBinding("g", .command),
            systemImage: "chevron.down",
            keywords: ["search", "next match"],
            isEnabled: { $0.center.find.hasMatches }
        ) { context in
            context.center.find.next()
        },

        AppCommand(
            id: "find.previous",
            title: "Find Previous",
            category: .edit,
            defaultKey: KeyBinding("g", [.command, .shift]),
            systemImage: "chevron.up",
            keywords: ["search", "previous match"],
            isEnabled: { $0.center.find.hasMatches }
        ) { context in
            context.center.find.previous()
        },

        AppCommand(
            id: "chat.copyLast",
            title: "Copy Last Response",
            category: .edit,
            defaultKey: KeyBinding("c", [.command, .shift]),
            systemImage: "doc.on.doc",
            keywords: ["clipboard", "pasteboard", "assistant"],
            isEnabled: { $0.stores.chat.lastAssistantText?.isEmpty == false }
        ) { context in
            guard let text = context.stores.chat.lastAssistantText, !text.isEmpty else { return }
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(text, forType: .string)
        },
    ]

    // MARK: - View

    static let view: [AppCommand] = [
        AppCommand(
            id: "view.toggleSidebar",
            title: "Toggle Sidebar",
            category: .view,
            defaultKey: KeyBinding("\\", .command),
            systemImage: "sidebar.leading",
            keywords: ["hide", "show", "collapse"]
        ) { context in
            let collapsed = !context.state.sidebarCollapsed
            context.state.sidebarCollapsed = collapsed
            try? await context.stores.settings.setSidebarCollapsed(collapsed)
        },

        AppCommand(
            id: "view.toggleAppearance",
            title: "Toggle Appearance",
            category: .view,
            defaultKey: KeyBinding("d", [.command, .shift]),
            systemImage: "circle.lefthalf.filled",
            keywords: ["dark", "light", "theme", "mode"]
        ) { context in
            // The controller repaints now; the settings write is what survives a relaunch.
            // `.core` is W9's bridge between the closed `FormDesign.ThemeMode` and the open
            // `FormCore.ThemeMode` — one conversion in the app, not one per caller.
            context.theme.toggleAppearance()
            try? await context.stores.settings.setThemeMode(context.theme.mode.core)
        },

        AppCommand(
            id: "view.textSizeIncrease",
            title: "Zoom In",
            category: .view,
            defaultKey: KeyBinding("+", .command),
            // A US layout produces `+` only with Shift, and `⌘=` is the equivalent every
            // Mac app also answers to. Both are declared rather than special-cased in the
            // matcher, so modifier comparison stays exact.
            alternateKeys: [KeyBinding("=", .command), KeyBinding("+", [.command, .shift])],
            systemImage: "textformat.size.larger",
            keywords: ["text size", "bigger", "zoom"]
        ) { context in
            await stepTextSize(context, by: 1)
        },

        AppCommand(
            id: "view.textSizeDecrease",
            title: "Zoom Out",
            category: .view,
            defaultKey: KeyBinding("-", .command),
            alternateKeys: [KeyBinding("_", [.command, .shift])],
            systemImage: "textformat.size.smaller",
            keywords: ["text size", "smaller", "zoom"]
        ) { context in
            await stepTextSize(context, by: -1)
        },

        AppCommand(
            id: "view.textSizeReset",
            title: "Actual Size",
            category: .view,
            defaultKey: KeyBinding("0", .command),
            systemImage: "textformat.size",
            keywords: ["text size", "reset", "default zoom"]
        ) { context in
            await setTextSize(context, to: 1.0)
        },
    ]

    // MARK: - Session

    static let session: [AppCommand] = [
        AppCommand(
            id: "chat.send",
            title: "Send Message",
            category: .session,
            defaultKey: KeyBinding(KeyBinding.returnKey, .command),
            systemImage: "arrow.up.circle",
            keywords: ["submit", "prompt", "run"],
            isEnabled: { $0.activeSessionId != nil }
        ) { context in
            // ⌘↩ works with the composer unfocused; the composer owns the draft, so it sends.
            context.hooks.submitComposer?()
        },

        AppCommand(
            id: "app.escape",
            title: "Stop or Dismiss",
            category: .session,
            defaultKey: KeyBinding(KeyBinding.escapeKey),
            systemImage: "escape",
            keywords: ["cancel", "abort", "close", "stop streaming"]
        ) { context in
            await context.center.handleEscape()
        },
    ]

    // MARK: - Navigate

    static let navigate: [AppCommand] = {
        var commands: [AppCommand] = [
            AppCommand(
                id: "palette.open",
                title: "Command Palette…",
                category: .navigate,
                defaultKey: KeyBinding("k", .command),
                systemImage: "magnifyingglass",
                keywords: ["search", "go to", "jump", "commands"]
            ) { context in
                context.center.togglePalette()
            },

            AppCommand(
                id: "nav.back",
                title: "Back",
                category: .navigate,
                defaultKey: KeyBinding("[", .command),
                alternateKeys: [KeyBinding(KeyBinding.leftArrow, [.command, .option])],
                systemImage: "chevron.left",
                keywords: ["previous", "history"],
                isEnabled: { $0.state.canGoBack }
            ) { context in
                context.state.goBack()
                await context.center.syncSelection()
            },

            AppCommand(
                id: "nav.forward",
                title: "Forward",
                category: .navigate,
                defaultKey: KeyBinding("]", .command),
                alternateKeys: [KeyBinding(KeyBinding.rightArrow, [.command, .option])],
                systemImage: "chevron.right",
                keywords: ["next", "history"],
                isEnabled: { $0.state.canGoForward }
            ) { context in
                context.state.goForward()
                await context.center.syncSelection()
            },

            AppCommand(
                id: "nav.home",
                title: "Home",
                category: .navigate,
                defaultKey: KeyBinding("h", [.command, .shift]),
                systemImage: "house",
                keywords: ["dashboard", "analytics", "stats"]
            ) { context in
                context.state.showHome()
            },
        ]

        // `⌘1`–`⌘9` index the sidebar's flattened *visible* order, which is why the rows
        // carry rank numbers (F2.1). Resolved through `SidebarOrder`, not
        // `SessionStore.session(rank:)`: the store sorts by pinned-then-`updatedAt`, which
        // discards the core's dense manual `index` and would make a dragged session snap
        // back. `SidebarOrder` is what the user is actually looking at, collapsed groups
        // included, so it is the only correct thing for a numbered jump to index into.
        for rank in 1...9 {
            commands.append(
                AppCommand(
                    id: "nav.session\(rank)",
                    title: "Session \(rank)",
                    category: .navigate,
                    defaultKey: KeyBinding(Character("\(rank)"), .command),
                    systemImage: "\(rank).square",
                    keywords: ["jump", "rank", "sidebar"],
                    isEnabled: {
                        SidebarOrder.session(rank: rank, in: $0.stores.sessions) != nil
                    }
                ) { context in
                    guard
                        let target = SidebarOrder.session(rank: rank, in: context.stores.sessions)
                    else { return }
                    context.state.showSession(target.id)
                    await context.stores.select(target.id)
                })
        }
        return commands
    }()

    // MARK: - Help

    static let help: [AppCommand] = [
        AppCommand(
            id: "help.cheatSheet",
            title: "Keyboard Shortcuts",
            category: .help,
            defaultKey: KeyBinding("/", .command),
            systemImage: "keyboard",
            keywords: ["cheat sheet", "keys", "bindings", "help"]
        ) { context in
            context.center.toggleCheatSheet()
        },
    ]

    // MARK: - Text size

    /// `⌘+` / `⌘-` walk `ThemeController.textScaleLadder` so the ends land exactly on the
    /// clamp values and the preferences slider snaps to the same stops (spec 08).
    @MainActor
    private static func stepTextSize(_ context: CommandContext, by direction: Int) async {
        let ladder = ThemeController.textScaleLadder
        let current = CGFloat(context.stores.settings.settings.appearance.textSizeMultiplier)
        let nearest = ladder.enumerated().min {
            abs($0.element - current) < abs($1.element - current)
        }?.offset ?? 0
        let next = ladder[min(ladder.count - 1, max(0, nearest + direction))]
        await setTextSize(context, to: next)
    }

    @MainActor
    private static func setTextSize(_ context: CommandContext, to value: CGFloat) async {
        context.theme.setTextScale(value)
        try? await context.stores.settings.setTextSizeMultiplier(Double(context.theme.textScale))
    }
}
