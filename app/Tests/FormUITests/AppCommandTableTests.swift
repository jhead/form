import Foundation
import Testing

@testable import FormUI

/// Spec 14 §1: "A test asserts: unique ids, no duplicate effective bindings after overrides,
/// every command reachable from a menu, and every entry in the PRD's F12 table present."
///
/// This is the test acceptance criterion 2 names, and the guard that keeps F12.3 — one table,
/// no duplicated key definitions — true as five other workstreams edit the same module.
@MainActor
struct AppCommandTableTests {
    // MARK: - The table itself

    @Test("command ids are unique")
    func idsAreUnique() {
        var seen = Set<String>()
        let duplicates = AppCommands.all.filter { !seen.insert($0.id).inserted }
        #expect(duplicates.isEmpty, "duplicate ids: \(duplicates.map(\.id))")
    }

    @Test("every command has a title and a namespaced id")
    func commandsAreWellFormed() {
        for command in AppCommands.all {
            #expect(!command.title.isEmpty, "\(command.id) has no title")
            let segments = command.id.components(separatedBy: ".")
            #expect(segments.count >= 2, "\(command.id) is not namespaced")
            for segment in segments {
                #expect(
                    segment.first?.isLowercase == true,
                    "\(command.id): each dot-separated segment is lowerCamel")
            }
        }
    }

    @Test("no two commands declare the same key equivalent")
    func defaultBindingsAreUnique() {
        var owner: [KeyBinding: String] = [:]
        var conflicts: [String] = []
        for command in AppCommands.all {
            for key in command.declaredKeys {
                if let existing = owner[key] {
                    conflicts.append("\(key.display) claimed by \(existing) and \(command.id)")
                } else {
                    owner[key] = command.id
                }
            }
        }
        #expect(conflicts.isEmpty, Comment(rawValue: conflicts.joined(separator: "\n")))
    }

    // MARK: - Effective bindings, with and without overrides

    @Test("the resolver produces no duplicate effective bindings")
    func effectiveBindingsAreUnique() {
        let resolver = ShortcutResolver()
        expectNoDuplicates(in: resolver)
    }

    @Test("a user override still leaves no duplicate effective bindings")
    func overriddenBindingsAreUnique() {
        // `⌘T` is unclaimed; `⌘N` is claimed by session.new. Both have to resolve cleanly.
        let resolver = ShortcutResolver(overrides: [
            "help.cheatSheet": "cmd+t",
            "nav.home": "cmd+n",
        ])
        expectNoDuplicates(in: resolver)

        #expect(resolver.primaryKey(for: "help.cheatSheet") == KeyBinding("t", .command))
        #expect(resolver.primaryKey(for: "nav.home") == KeyBinding("n", .command))
        // The override took ⌘N, so the default owner lost it and says so.
        #expect(resolver.primaryKey(for: "session.new") == nil)
        #expect(resolver.displacedKeys(for: "session.new") == [KeyBinding("n", .command)])
    }

    @Test("an override written in glyph form resolves the same as token form")
    func overridesParseBothForms() {
        let glyph = ShortcutResolver(overrides: ["nav.home": "⌘⌃H"])
        let token = ShortcutResolver(overrides: ["nav.home": "cmd+ctrl+h"])
        #expect(glyph.primaryKey(for: "nav.home") == token.primaryKey(for: "nav.home"))
        #expect(glyph.primaryKey(for: "nav.home") == KeyBinding("h", [.command, .control]))
    }

    @Test("an empty override unbinds a command without freeing it for a duplicate")
    func emptyOverrideUnbinds() {
        let resolver = ShortcutResolver(overrides: ["session.new": ""])
        #expect(resolver.primaryKey(for: "session.new") == nil)
        #expect(resolver.keys(for: "session.new").isEmpty)
        expectNoDuplicates(in: resolver)
    }

    private func expectNoDuplicates(in resolver: ShortcutResolver) {
        var owner: [KeyBinding: String] = [:]
        var conflicts: [String] = []
        for command in AppCommands.all {
            for key in resolver.keys(for: command.id) {
                if let existing = owner[key] {
                    conflicts.append("\(key.display): \(existing) and \(command.id)")
                } else {
                    owner[key] = command.id
                }
            }
        }
        #expect(conflicts.isEmpty, Comment(rawValue: conflicts.joined(separator: "\n")))
    }

    // MARK: - Menu coverage

    @Test("every command is reachable from a menu")
    func everyCommandIsInAMenu() {
        let inMenus = Set(AppCommandMenus.menuCommandIDs)
        let missing = AppCommands.all.map(\.id).filter { !inMenus.contains($0) }
        #expect(missing.isEmpty, "not reachable from any menu: \(missing)")
    }

    @Test("no menu names a command that does not exist, and none appears twice")
    func menuLayoutIsConsistent() {
        let ids = AppCommandMenus.menuCommandIDs
        let unknown = ids.filter { AppCommands.command(id: $0) == nil }
        #expect(unknown.isEmpty, "menus name unknown commands: \(unknown)")

        var seen = Set<String>()
        let duplicated = ids.filter { !seen.insert($0).inserted }
        #expect(duplicated.isEmpty, "listed in more than one menu: \(duplicated)")
    }

    // MARK: - PRD F12

    /// Every row of the PRD §5 F12 table, plus the two spec 14 §2/§5 additions. Written out
    /// literally rather than derived, so this fails if a binding is quietly dropped.
    static let f12Bindings: [(keys: String, action: String)] = [
        ("cmd+n", "New chat"),
        ("cmd+shift+n", "New chat in current group"),
        ("cmd+[", "Previous session"),
        ("cmd+]", "Next session"),
        ("cmd+opt+left", "Previous session, alternate binding"),
        ("cmd+opt+right", "Next session, alternate binding"),
        ("cmd+1", "Jump to session 1"),
        ("cmd+2", "Jump to session 2"),
        ("cmd+3", "Jump to session 3"),
        ("cmd+4", "Jump to session 4"),
        ("cmd+5", "Jump to session 5"),
        ("cmd+6", "Jump to session 6"),
        ("cmd+7", "Jump to session 7"),
        ("cmd+8", "Jump to session 8"),
        ("cmd+9", "Jump to session 9"),
        ("cmd+k", "Command palette / session search"),
        ("cmd+f", "Find in current session"),
        ("cmd+g", "Find next"),
        ("cmd+shift+g", "Find previous"),
        ("cmd+\\", "Toggle sidebar"),
        ("cmd+,", "Preferences"),
        ("cmd+shift+h", "Home"),
        ("cmd+w", "Close (archive) session"),
        ("cmd+shift+k", "Clear / new from current"),
        ("cmd+return", "Send"),
        ("escape", "Stop streaming / dismiss overlay"),
        ("cmd+shift+c", "Copy last response"),
        ("cmd+shift+d", "Toggle appearance"),
        ("cmd+shift+f", "Choose workspace folder"),
        ("cmd+0", "Reset text size"),
        ("cmd++", "Zoom in"),
        ("cmd+-", "Zoom out"),
        // spec 14 §2 — ⌘W archives, so closing the window needs its own key.
        ("cmd+shift+w", "Close window"),
        // F12.2 — the cheat sheet.
        ("cmd+/", "Shortcut cheat sheet"),
    ]

    @Test("every binding in the PRD's F12 table is declared")
    func f12TableIsComplete() {
        var declared: [KeyBinding: String] = [:]
        for command in AppCommands.all {
            for key in command.declaredKeys { declared[key] = command.id }
        }

        var missing: [String] = []
        for entry in Self.f12Bindings {
            guard let binding = KeyBinding(parsing: entry.keys) else {
                missing.append("\(entry.keys) — the test's own spelling does not parse")
                continue
            }
            if declared[binding] == nil {
                missing.append("\(binding.display) (\(entry.action)) is not bound by any command")
            }
        }
        #expect(missing.isEmpty, Comment(rawValue: missing.joined(separator: "\n")))
    }

    @Test("F12 bindings survive into the resolver's effective table")
    func f12TableIsLive() {
        let resolver = ShortcutResolver()
        let effective = Set(resolver.allEffectiveKeys)
        var missing: [String] = []
        for entry in Self.f12Bindings {
            guard let binding = KeyBinding(parsing: entry.keys) else { continue }
            if !effective.contains(binding) {
                missing.append("\(binding.display) (\(entry.action))")
            }
        }
        #expect(missing.isEmpty, "declared but not effective: \(missing)")
    }

    @Test("every F12 binding is reachable from the menu bar")
    func f12BindingsAppearInMenus() {
        let resolver = ShortcutResolver()
        let menuIDs = Set(AppCommandMenus.menuCommandIDs)
        var missing: [String] = []
        for entry in Self.f12Bindings {
            guard let binding = KeyBinding(parsing: entry.keys) else { continue }
            let owner = AppCommands.all.first { command in
                resolver.keys(for: command.id).contains(binding)
            }
            guard let owner else {
                missing.append("\(binding.display) has no owning command")
                continue
            }
            if !menuIDs.contains(owner.id) {
                missing.append("\(binding.display) → \(owner.id) is in no menu")
            }
        }
        #expect(missing.isEmpty, Comment(rawValue: missing.joined(separator: "\n")))
    }

    // MARK: - One table, enforced

    /// F12.3: "Shortcuts are declared in one table consumed by both menus and handlers — no
    /// duplicated key definitions." Review across six parallel workstreams cannot enforce
    /// that; a grep can.
    @Test("no file outside the menu generator declares a keyboardShortcut")
    func onlyTheMenuGeneratorBindsKeys() throws {
        let root = try Self.appRoot()
        let allowed = ["Sources/FormUI/Commands/KeyBinding.swift"]
        var violations: [String] = []
        var scanned = 0

        for directory in ["Sources/FormUI", "Sources/FormDesign", "Sources/FormMarkdown", "Sources/form"] {
            let url = root.appending(path: directory)
            guard FileManager.default.fileExists(atPath: url.path) else { continue }
            for file in try Self.swiftFiles(in: url) {
                let relative = file.path.replacingOccurrences(of: root.path + "/", with: "")
                guard !allowed.contains(relative) else { continue }
                scanned += 1
                let source = try String(contentsOf: file, encoding: .utf8)
                for (index, line) in source.components(separatedBy: .newlines).enumerated() {
                    let code = line.components(separatedBy: "//").first ?? line
                    guard code.contains(".keyboardShortcut(") || code.contains("keyboardShortcut(") else {
                        continue
                    }
                    violations.append("  \(relative):\(index + 1)  \(line.trimmingCharacters(in: .whitespaces))")
                }
            }
        }

        #expect(
            violations.isEmpty,
            """
            \(violations.count) key equivalent(s) declared outside the table (F12.3):

            \(violations.joined(separator: "\n"))

            Add a row to AppCommands.all and let the menu generator bind it.
            """)
        #expect(scanned > 0, "scanned no Swift files; did Sources/ move? \(root.path)")
    }

    // MARK: - Helpers

    private static func swiftFiles(in directory: URL) throws -> [URL] {
        guard
            let enumerator = FileManager.default.enumerator(
                at: directory, includingPropertiesForKeys: [.isRegularFileKey],
                options: [.skipsHiddenFiles])
        else { return [] }
        return enumerator.compactMap { element in
            guard let url = element as? URL, url.pathExtension == "swift" else { return nil }
            return url
        }
    }

    /// Walks up to `app/`. Symlinks are resolved first so the same test works from a mirror
    /// package as from the real one.
    static func appRoot(from file: StaticString = #filePath) throws -> URL {
        var url = URL(fileURLWithPath: "\(file)")
            .resolvingSymlinksInPath()
            .deletingLastPathComponent()  // FormUITests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // app
        var climbs = 0
        while !FileManager.default.fileExists(atPath: url.appending(path: "Sources/FormUI").path),
              climbs < 4 {
            url = url.deletingLastPathComponent()
            climbs += 1
        }
        guard FileManager.default.fileExists(atPath: url.appending(path: "Sources/FormUI").path)
        else { throw TableTestError.rootNotFound("\(file)") }
        return url
    }

    enum TableTestError: Error, CustomStringConvertible {
        case rootNotFound(String)
        var description: String {
            switch self {
            case let .rootNotFound(path): "could not locate app/ above \(path)"
            }
        }
    }
}
