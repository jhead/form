import AppKit
import SwiftUI

/// The four modifiers a `form` binding may use, in Apple's canonical display order.
///
/// This is deliberately not `SwiftUI.EventModifiers`: that type carries `.numericPad` and
/// `.capsLock`, has no stable serialization, and cannot be compared against an `NSEvent`
/// without normalizing first. One small type owns all three representations instead, so the
/// menu bar, the key monitor and `settings.shortcuts` cannot drift apart.
public struct KeyModifiers: OptionSet, Sendable, Hashable, Codable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }

    // Bit order is display order: ⌃⌥⇧⌘.
    public static let control = KeyModifiers(rawValue: 1 << 0)
    public static let option = KeyModifiers(rawValue: 1 << 1)
    public static let shift = KeyModifiers(rawValue: 1 << 2)
    public static let command = KeyModifiers(rawValue: 1 << 3)

    static let ordered: [(KeyModifiers, glyph: String, token: String)] = [
        (.control, "⌃", "ctrl"),
        (.option, "⌥", "opt"),
        (.shift, "⇧", "shift"),
        (.command, "⌘", "cmd"),
    ]

    /// `"⌘⇧"` — the prefix a key equivalent renders with.
    public var glyphs: String {
        Self.ordered.reduce(into: "") { result, entry in
            if contains(entry.0) { result += entry.glyph }
        }
    }

    /// `"cmd+shift"` — what goes into `settings.shortcuts`.
    public var tokens: [String] {
        Self.ordered.compactMap { contains($0.0) ? $0.token : nil }
    }

    /// Spoken form, for VoiceOver and the cheat sheet's accessibility label.
    public var spokenNames: [String] {
        Self.ordered.compactMap { entry in
            guard contains(entry.0) else { return nil }
            switch entry.0 {
            case .control: return "Control"
            case .option: return "Option"
            case .shift: return "Shift"
            default: return "Command"
            }
        }
    }

    public var eventModifiers: EventModifiers {
        var result: EventModifiers = []
        if contains(.command) { result.insert(.command) }
        if contains(.shift) { result.insert(.shift) }
        if contains(.option) { result.insert(.option) }
        if contains(.control) { result.insert(.control) }
        return result
    }

    public var flags: NSEvent.ModifierFlags {
        var result: NSEvent.ModifierFlags = []
        if contains(.command) { result.insert(.command) }
        if contains(.shift) { result.insert(.shift) }
        if contains(.option) { result.insert(.option) }
        if contains(.control) { result.insert(.control) }
        return result
    }

    /// Only the four modifiers this app binds; everything else an `NSEvent` reports (caps
    /// lock, numeric pad, function) is noise for matching purposes.
    public init(_ flags: NSEvent.ModifierFlags) {
        var result: KeyModifiers = []
        if flags.contains(.command) { result.insert(.command) }
        if flags.contains(.shift) { result.insert(.shift) }
        if flags.contains(.option) { result.insert(.option) }
        if flags.contains(.control) { result.insert(.control) }
        self = result
    }

    /// Parses one token: a word (`cmd`, `option`, …) or a glyph (`⌘`, `⌥`, …).
    static func parse(token: String) -> KeyModifiers? {
        switch token.lowercased() {
        case "cmd", "command", "meta", "⌘": return .command
        case "shift", "⇧": return .shift
        case "opt", "option", "alt", "⌥": return .option
        case "ctrl", "control", "⌃": return .control
        default: return nil
        }
    }
}

/// One key equivalent: a base key plus modifiers.
///
/// The base key is stored as the `Character` that both `KeyEquivalent` and
/// `NSEvent.charactersIgnoringModifiers` use, so the same value drives the menu item and the
/// event monitor. Function keys line up because AppKit's `NSUpArrowFunctionKey` family and
/// SwiftUI's `KeyEquivalent.upArrow` family are the same private-use scalars.
public struct KeyBinding: Sendable, Hashable, Codable, CustomStringConvertible {
    /// Lowercased for letters, so `⌘N` and `⌘n` are the same binding.
    public let key: Character
    public let modifiers: KeyModifiers

    public init(_ key: Character, _ modifiers: KeyModifiers = []) {
        self.key = Character(String(key).lowercased())
        self.modifiers = modifiers
    }

    // MARK: - Named keys

    /// Non-printing keys, by the name they serialize under and the glyph they display as.
    static let namedKeys: [(name: String, glyph: String, spoken: String, character: Character)] = [
        ("left", "←", "Left Arrow", Character(UnicodeScalar(0xF702) ?? " ")),
        ("right", "→", "Right Arrow", Character(UnicodeScalar(0xF703) ?? " ")),
        ("up", "↑", "Up Arrow", Character(UnicodeScalar(0xF700) ?? " ")),
        ("down", "↓", "Down Arrow", Character(UnicodeScalar(0xF701) ?? " ")),
        ("return", "⏎", "Return", "\r"),
        ("escape", "⎋", "Escape", "\u{1B}"),
        ("tab", "⇥", "Tab", "\t"),
        ("space", "␣", "Space", " "),
        ("delete", "⌫", "Delete", "\u{7F}"),
    ]

    public static let leftArrow = Character(UnicodeScalar(0xF702) ?? " ")
    public static let rightArrow = Character(UnicodeScalar(0xF703) ?? " ")
    public static let upArrow = Character(UnicodeScalar(0xF700) ?? " ")
    public static let downArrow = Character(UnicodeScalar(0xF701) ?? " ")
    public static let returnKey: Character = "\r"
    public static let escapeKey: Character = "\u{1B}"

    // MARK: - Parsing

    /// Accepts both serialized form (`"cmd+shift+n"`, `"cmd+left"`) and rendered form
    /// (`"⌘⇧N"`, `"⌘⌥←"`). The Shortcuts preferences tab records one and the settings
    /// document may already hold the other, so both have to round-trip.
    public init?(parsing input: String) {
        let trimmed = input.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return nil }

        var modifiers: KeyModifiers = []
        var remainder = trimmed

        // Token form. `+` is both the separator and a legal key, so a trailing `+` is read
        // as the key and everything before it as modifiers — that is what makes "cmd++"
        // (the serialized form of `⌘+`) round-trip.
        if trimmed.count > 1, trimmed.contains("+") {
            var tokens: [String]
            var keyPart: String?
            if trimmed.hasSuffix("+") {
                keyPart = "+"
                tokens = String(trimmed.dropLast()).components(separatedBy: "+")
                    .filter { !$0.isEmpty }
            } else {
                var parts = trimmed.components(separatedBy: "+").filter { !$0.isEmpty }
                if parts.count > 1 {
                    keyPart = parts.removeLast()
                    tokens = parts
                } else {
                    tokens = []
                }
            }
            if let keyPart, !tokens.isEmpty {
                let parsed = tokens.compactMap(KeyModifiers.parse(token:))
                // All-or-nothing: a stray token means this was not token form after all, so
                // fall through and let the glyph reader have it.
                if parsed.count == tokens.count {
                    modifiers = parsed.reduce(into: []) { $0.insert($1) }
                    remainder = keyPart
                }
            }
        }

        // Glyph form: strip leading modifier glyphs.
        while let first = remainder.first, let modifier = KeyModifiers.parse(token: String(first)),
              remainder.count > 1 {
            modifiers.insert(modifier)
            remainder.removeFirst()
        }

        let name = remainder.lowercased()
        if let named = Self.namedKeys.first(where: { $0.name == name || $0.glyph == remainder }) {
            self.init(named.character, modifiers)
            return
        }
        switch name {
        case "plus": self.init("+", modifiers); return
        case "minus": self.init("-", modifiers); return
        case "esc": self.init(Self.escapeKey, modifiers); return
        case "enter": self.init(Self.returnKey, modifiers); return
        default: break
        }
        guard remainder.count == 1, let character = remainder.first else { return nil }
        self.init(character, modifiers)
    }

    // MARK: - Rendering

    /// `"⌘⇧N"` — what the menu bar and the cheat sheet show.
    public var display: String {
        modifiers.glyphs + keyGlyph
    }

    public var description: String { display }

    var keyGlyph: String {
        if let named = Self.namedKeys.first(where: { $0.character == key }) { return named.glyph }
        return String(key).uppercased()
    }

    /// `"cmd+shift+n"` — the form written back to `settings.shortcuts`.
    public var serialized: String {
        (modifiers.tokens + [keyToken]).joined(separator: "+")
    }

    var keyToken: String {
        if let named = Self.namedKeys.first(where: { $0.character == key }) { return named.name }
        return String(key)
    }

    /// `"Command Shift N"`, for VoiceOver.
    public var spokenDescription: String {
        let keyName = Self.namedKeys.first { $0.character == key }?.spoken
            ?? String(key).uppercased()
        return (modifiers.spokenNames + [keyName]).joined(separator: " ")
    }

    // MARK: - SwiftUI and AppKit

    public var keyEquivalent: KeyEquivalent { KeyEquivalent(key) }
    public var eventModifiers: EventModifiers { modifiers.eventModifiers }

    /// Exact match: the four tracked modifiers must be identical, not merely present. That
    /// is what keeps `⌘G` and `⌘⇧G` from both firing on `⌘⇧G`.
    public func matches(_ event: NSEvent) -> Bool {
        guard event.type == .keyDown else { return false }
        guard KeyModifiers(event.modifierFlags) == modifiers else { return false }
        return matches(characters: event.charactersIgnoringModifiers)
            || matches(characters: event.characters)
    }

    private func matches(characters: String?) -> Bool {
        guard let characters, let first = characters.first else { return false }
        return Character(String(first).lowercased()) == key
    }

    // MARK: - Codable

    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        guard let parsed = KeyBinding(parsing: raw) else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "not a key equivalent: \(raw)")
        }
        self = parsed
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(serialized)
    }
}

public extension View {
    /// The only place a `KeyBinding` becomes a SwiftUI shortcut. Menu items call it; nothing
    /// else in the app is allowed to call `.keyboardShortcut` directly (F12.3).
    func keyBinding(_ binding: KeyBinding?) -> some View {
        modifier(KeyBindingModifier(binding: binding))
    }
}

private struct KeyBindingModifier: ViewModifier {
    let binding: KeyBinding?

    func body(content: Content) -> some View {
        if let binding {
            content.keyboardShortcut(binding.keyEquivalent, modifiers: binding.eventModifiers)
        } else {
            content
        }
    }
}
