import SwiftUI

/// One entry in a generated menu.
public indirect enum MenuItem: Sendable {
    case command(String)
    case separator
    case submenu(String, [MenuItem])

    var commandIDs: [String] {
        switch self {
        case let .command(id): [id]
        case .separator: []
        case let .submenu(_, items): items.flatMap(\.commandIDs)
        }
    }
}

/// Where a generated section attaches in the macOS menu bar.
public enum MenuPlacement: Sendable, Equatable {
    /// Replaces one of SwiftUI's standard groups — used where the default item would
    /// otherwise duplicate one of ours (`.newItem` is New Window; `.saveItem` is Close).
    case replacing(String)
    case after(String)
    /// A top-level menu of our own.
    case menu(String)
}

/// A section of the menu bar, declared as data.
///
/// The menu bar is generated from this, and the table test walks it to prove every command
/// is reachable from a menu (F12.1, spec 14 §1) — an assertion no amount of reading a
/// `some Commands` body could make.
public struct MenuSection: Identifiable, Sendable {
    public let id: String
    public let placement: MenuPlacement
    public let items: [MenuItem]

    public var commandIDs: [String] { items.flatMap(\.commandIDs) }
}

/// The app's menu bar, generated from `AppCommands.all` (F12.1).
///
/// **This is the only place in the app that binds a key to an action** — through
/// `keyBinding(_:)`, the single wrapper over SwiftUI's key-equivalent modifier, whose value
/// comes from the resolver so a user override shows up in the menu too (F12.3).
@MainActor
public struct AppCommandMenus: Commands {
    private let center: CommandCenter

    public init(center: CommandCenter) {
        self.center = center
    }

    public static let layout: [MenuSection] = [
        MenuSection(
            id: "file.new",
            placement: .replacing("newItem"),
            items: [
                .command("session.new"),
                .command("session.newInGroup"),
                .command("session.newFromCurrent"),
            ]),
        MenuSection(
            id: "file.session",
            placement: .replacing("saveItem"),
            items: [
                .command("workspace.choose"),
                .separator,
                .command("session.archive"),
                .command("window.close"),
            ]),
        MenuSection(
            id: "app.settings",
            placement: .replacing("appSettings"),
            items: [.command("app.preferences")]),
        MenuSection(
            id: "edit.find",
            placement: .after("pasteboard"),
            items: [
                .command("find.open"),
                .command("find.next"),
                .command("find.previous"),
                .separator,
                .command("chat.copyLast"),
            ]),
        MenuSection(
            id: "view.appearance",
            placement: .after("sidebar"),
            items: [
                .command("view.toggleSidebar"),
                .command("view.toggleAppearance"),
                .separator,
                .command("view.textSizeIncrease"),
                .command("view.textSizeDecrease"),
                .command("view.textSizeReset"),
            ]),
        MenuSection(
            id: "session",
            placement: .menu("Session"),
            items: [
                .command("chat.send"),
                .command("app.escape"),
            ]),
        MenuSection(
            id: "navigate",
            placement: .menu("Navigate"),
            items: [
                .command("palette.open"),
                .separator,
                .command("nav.back"),
                .command("nav.forward"),
                .command("nav.home"),
                .separator,
                .submenu("Go to Session", (1...9).map { .command("nav.session\($0)") }),
            ]),
        MenuSection(
            id: "help",
            placement: .replacing("help"),
            items: [.command("help.cheatSheet")]),
    ]

    /// Every command the menu bar exposes, in menu order.
    public static var menuCommandIDs: [String] { layout.flatMap(\.commandIDs) }

    public var body: some Commands {
        CommandGroup(replacing: .newItem) { section("file.new") }
        CommandGroup(replacing: .saveItem) { section("file.session") }
        CommandGroup(replacing: .appSettings) { section("app.settings") }
        CommandGroup(after: .pasteboard) { section("edit.find") }
        CommandGroup(after: .sidebar) { section("view.appearance") }
        CommandMenu("Session") { section("session") }
        CommandMenu("Navigate") { section("navigate") }
        CommandGroup(replacing: .help) { section("help") }
    }

    @ViewBuilder
    private func section(_ id: String) -> some View {
        if let section = Self.layout.first(where: { $0.id == id }) {
            ForEach(Array(section.items.enumerated()), id: \.offset) { _, item in
                MenuItemView(item: item, center: center)
            }
        }
    }
}

/// Renders one `MenuItem`. Recursion lives in a view rather than a builder so a submenu is
/// declared the same way a top-level item is.
private struct MenuItemView: View {
    let item: MenuItem
    let center: CommandCenter

    var body: some View {
        switch item {
        case let .command(id):
            if let command = AppCommands.command(id: id) {
                Button(command.title) { center.perform(command) }
                    .keyBinding(center.resolver.primaryKey(for: id))
                    .disabled(!center.isEnabled(command))
            }
        case .separator:
            Divider()
        case let .submenu(title, items):
            Menu(title) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, child in
                    MenuItemView(item: child, center: center)
                }
            }
        }
    }
}
