import AppKit
import FormCore
import FormDesign
import SwiftUI

/// Menu grouping for the table (spec 14 §1). The order of the cases is the order the menu
/// bar and the cheat sheet present them in.
public enum CommandCategory: String, CaseIterable, Sendable, Identifiable, Codable {
    case file, edit, view, session, navigate, help

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .file: "File"
        case .edit: "Edit"
        case .view: "View"
        case .session: "Session"
        case .navigate: "Navigate"
        case .help: "Help"
        }
    }
}

/// One declared action. **This struct plus `AppCommands.all` is the single shortcut table**
/// (F12.3): the menu bar, the global key monitor, the `⌘K` palette, the `⌘/` cheat sheet and
/// W13's Shortcuts preferences tab all read it, and nothing else in the app declares a key
/// equivalent.
public struct AppCommand: Identifiable, Sendable {
    public let id: String
    public let title: String
    public let category: CommandCategory
    public let defaultKey: KeyBinding?
    /// Extra equivalents that fire the same action but are not what the menu shows — the
    /// `⌘⌥←` / `⌘⌥→` aliases of `⌘[` / `⌘]`, and `⌘=` for `⌘+` (spec 14 §2). Kept separate
    /// from `defaultKey` so a menu item still has exactly one key equivalent, while the
    /// uniqueness test still sees every equivalent the app will respond to.
    public let alternateKeys: [KeyBinding]
    /// SF Symbol for the palette row. Purely decorative.
    public let systemImage: String?
    /// Extra words the palette's fuzzy match should consider — a user looking for "dark
    /// mode" should find "Toggle Appearance".
    public let keywords: [String]
    public let isEnabled: @MainActor @Sendable (CommandContext) -> Bool
    public let perform: @MainActor @Sendable (CommandContext) async -> Void

    public init(
        id: String,
        title: String,
        category: CommandCategory,
        defaultKey: KeyBinding? = nil,
        alternateKeys: [KeyBinding] = [],
        systemImage: String? = nil,
        keywords: [String] = [],
        isEnabled: @escaping @MainActor @Sendable (CommandContext) -> Bool = { _ in true },
        perform: @escaping @MainActor @Sendable (CommandContext) async -> Void
    ) {
        self.id = id
        self.title = title
        self.category = category
        self.defaultKey = defaultKey
        self.alternateKeys = alternateKeys
        self.systemImage = systemImage
        self.keywords = keywords
        self.isEnabled = isEnabled
        self.perform = perform
    }

    /// Every equivalent declared by default, primary first.
    public var declaredKeys: [KeyBinding] {
        (defaultKey.map { [$0] } ?? []) + alternateKeys
    }
}

/// Everything a command needs to do its work. Handed to `perform` rather than captured, so
/// the table stays a value that can be built once and tested without an app.
@MainActor
public struct CommandContext {
    public let stores: CoreStores
    public let theme: ThemeController
    public let state: any CommandAppState
    public let center: CommandCenter

    public var hooks: CommandHooks { center.hooks }

    public init(
        stores: CoreStores,
        theme: ThemeController,
        state: any CommandAppState,
        center: CommandCenter
    ) {
        self.stores = stores
        self.theme = theme
        self.state = state
        self.center = center
    }

    /// The session a session-scoped command acts on: whatever is routed to, falling back to
    /// the sidebar selection so `⌘W` still works the instant after a jump.
    public var activeSessionId: String? {
        state.currentSessionId ?? stores.sessions.selectedSessionId
    }

    public var activeSession: SessionSummary? {
        activeSessionId.flatMap { stores.sessions.session(id: $0) }
    }
}

/// The seams other workstreams fill in.
///
/// A handful of F12 actions live in directories W14 does not own — the composer's draft
/// (W10), the preferences sheet and the folder picker (W13). Rather than reach across an
/// ownership boundary, the table calls a closure and the owning workstream installs it at
/// startup. An unset hook is a no-op, so the app is never broken by a missing one.
@MainActor
public struct CommandHooks {
    /// `⌘↩` — send the composer's current draft. W10.
    public var submitComposer: (() -> Void)?
    /// Give the composer keyboard focus. W10.
    public var focusComposer: (() -> Void)?
    /// `Esc`, last resort — resign composer focus. Returns `true` if it had focus. W10.
    public var clearComposerFocus: (() -> Bool)?
    /// The composer's or transcript's current text selection, used to seed `⌘F`. W10.
    public var selectedText: (() -> String?)?
    /// `⌘,` — present the preferences sheet. W13.
    public var openPreferences: ((String?) -> Void)?
    /// `⌘⇧F` — run the workspace folder picker. W13.
    public var chooseWorkspaceFolder: (() -> Void)?

    public init() {}
}
