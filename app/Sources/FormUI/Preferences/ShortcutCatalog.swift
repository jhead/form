import AppKit
import Foundation

/// One row of the shortcut table, as the Shortcuts tab needs it.
///
/// **This is the seam with W14, not a second copy of their table.** `FormUI/Commands` owns
/// `AppCommands.all` — the single table the menu bar, the key handler and the palette read
/// (spec 14 §1) — and publishes it here by setting `ShortcutCatalog.source` once at startup.
/// Preferences never enumerates commands itself: with no source registered the tab says so
/// rather than inventing rows that would drift from the real bindings.
public struct ShortcutDescriptor: Identifiable, Sendable, Equatable {
    public let id: String
    public let title: String
    /// Free text, used only for grouping and section titles — `File`, `View`, `Session`…
    public let category: String
    /// The built-in binding in canonical form (`KeyBindingText`), or `nil` for a command with
    /// no default key.
    public let defaultKey: String?

    public init(id: String, title: String, category: String, defaultKey: String?) {
        self.id = id
        self.title = title
        self.category = category
        self.defaultKey = defaultKey
    }
}

@MainActor
public enum ShortcutCatalog {
    /// Set once by W14. A closure rather than an array so the table stays theirs and is read
    /// fresh — a command whose availability changes must not need a re-registration.
    public static var source: (@MainActor () -> [ShortcutDescriptor])?

    public static var all: [ShortcutDescriptor] { source?() ?? [] }

    /// Categories in first-seen order, so the tab's section order is the table's order rather
    /// than an alphabetical one nobody chose.
    public static func grouped(_ rows: [ShortcutDescriptor]) -> [(String, [ShortcutDescriptor])] {
        var order: [String] = []
        var byCategory: [String: [ShortcutDescriptor]] = [:]
        for row in rows {
            if byCategory[row.category] == nil { order.append(row.category) }
            byCategory[row.category, default: []].append(row)
        }
        return order.map { ($0, byCategory[$0] ?? []) }
    }
}

/// The on-disk form of a key equivalent.
///
/// `settings.shortcuts` is `action id -> key equivalent` and `settings.json` is meant to be
/// hand-editable (spec 04 §2), so the stored form is ASCII tokens — `cmd+shift+n` — not the
/// `⌘⇧N` glyphs. Glyphs are for display only. Parsing is tolerant of order, case and spacing
/// so a hand-written value works; `canonical` is what the recorder writes back.
public enum KeyBindingText {
    public struct Binding: Equatable {
        public var modifiers: NSEvent.ModifierFlags
        /// Lowercased key token: a single character, a digit, or a name from `namedKeys`.
        public var key: String

        public init(modifiers: NSEvent.ModifierFlags, key: String) {
            self.modifiers = modifiers
            self.key = key
        }
    }

    /// Keys with no printable single-character form, and the few printable ones whose glyph
    /// would be ambiguous in a settings file (`+` reads as a separator).
    static let namedKeys: [String: String] = [
        "left": "←", "right": "→", "up": "↑", "down": "↓",
        "return": "⏎", "enter": "⌤", "escape": "⎋", "delete": "⌫", "forwarddelete": "⌦",
        "space": "␣", "tab": "⇥", "home": "↖", "end": "↘", "pageup": "⇞", "pagedown": "⇟",
        "plus": "+", "minus": "−", "equal": "=", "comma": ",", "period": ".", "slash": "/",
        "backslash": "\\", "leftbracket": "[", "rightbracket": "]", "grave": "`",
        "semicolon": ";", "quote": "'",
    ]

    private static func modifier(for token: String) -> NSEvent.ModifierFlags? {
        switch token {
        case "cmd", "command": .command
        case "shift": .shift
        case "opt", "option", "alt": .option
        case "ctrl", "control": .control
        default: nil
        }
    }

    public static func parse(_ text: String) -> Binding? {
        let parts = text.lowercased()
            .replacingOccurrences(of: " ", with: "")
            .split(separator: "+", omittingEmptySubsequences: false)
            .map(String.init)
        guard !parts.isEmpty else { return nil }

        var modifiers: NSEvent.ModifierFlags = []
        var key: String?

        for (index, part) in parts.enumerated() {
            // A trailing empty component is the `+` key written literally: `cmd++`.
            let token = part.isEmpty ? (index == parts.count - 1 ? "plus" : "") : part
            guard !token.isEmpty else { continue }
            if let flag = modifier(for: token) {
                modifiers.insert(flag)
            } else {
                key = token
            }
        }

        guard let key, !key.isEmpty else { return nil }
        return Binding(modifiers: modifiers, key: key)
    }

    /// The stored form: modifiers in a fixed order so two equal bindings compare equal as
    /// strings, which is what conflict detection relies on.
    public static func canonical(_ binding: Binding) -> String {
        var tokens: [String] = []
        if binding.modifiers.contains(.control) { tokens.append("ctrl") }
        if binding.modifiers.contains(.option) { tokens.append("opt") }
        if binding.modifiers.contains(.shift) { tokens.append("shift") }
        if binding.modifiers.contains(.command) { tokens.append("cmd") }
        tokens.append(binding.key)
        return tokens.joined(separator: "+")
    }

    public static func canonical(_ text: String) -> String? {
        parse(text).map(canonical)
    }

    /// `⌃⌥⇧⌘N` — display only. An unparseable stored value renders as itself so a typo is
    /// visible rather than blank.
    public static func display(_ text: String) -> String {
        guard let binding = parse(text) else { return text }
        var glyphs = ""
        if binding.modifiers.contains(.control) { glyphs += "⌃" }
        if binding.modifiers.contains(.option) { glyphs += "⌥" }
        if binding.modifiers.contains(.shift) { glyphs += "⇧" }
        if binding.modifiers.contains(.command) { glyphs += "⌘" }
        return glyphs + (namedKeys[binding.key] ?? binding.key.uppercased())
    }

    /// Builds the stored form from a captured event.
    public static func binding(from event: NSEvent) -> Binding? {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        var modifiers: NSEvent.ModifierFlags = []
        for flag in [NSEvent.ModifierFlags.command, .shift, .option, .control]
        where flags.contains(flag) {
            modifiers.insert(flag)
        }
        guard let key = keyToken(for: event) else { return nil }
        return Binding(modifiers: modifiers, key: key)
    }

    private static func keyToken(for event: NSEvent) -> String? {
        switch event.keyCode {
        case 123: return "left"
        case 124: return "right"
        case 126: return "up"
        case 125: return "down"
        case 36: return "return"
        case 76: return "enter"
        case 53: return "escape"
        case 51: return "delete"
        case 117: return "forwarddelete"
        case 49: return "space"
        case 48: return "tab"
        case 115: return "home"
        case 119: return "end"
        case 116: return "pageup"
        case 121: return "pagedown"
        default: break
        }
        // `charactersIgnoringModifiers` is the unshifted character, which is what makes
        // `⇧/` record as `shift+/` rather than as `?`.
        guard let raw = event.charactersIgnoringModifiers?.lowercased(), let scalar = raw.first
        else { return nil }
        switch scalar {
        case "+": return "plus"
        case "-": return "minus"
        case "=": return "equal"
        case ",": return "comma"
        case ".": return "period"
        case "/": return "slash"
        case "\\": return "backslash"
        case "[": return "leftbracket"
        case "]": return "rightbracket"
        case "`": return "grave"
        case ";": return "semicolon"
        case "'": return "quote"
        default:
            guard scalar.isLetter || scalar.isNumber else { return nil }
            return String(scalar)
        }
    }
}
