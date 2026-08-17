import AppKit
import Testing

@testable import FormUI

/// The type both the menu bar and the key monitor read. If parsing, rendering and matching
/// ever disagree, F12.3 quietly stops being true — so all three are pinned here.
@MainActor
struct KeyBindingTests {
    @Test("token form parses")
    func parsesTokenForm() {
        #expect(KeyBinding(parsing: "cmd+n") == KeyBinding("n", .command))
        #expect(KeyBinding(parsing: "cmd+shift+n") == KeyBinding("n", [.command, .shift]))
        #expect(KeyBinding(parsing: "command+option+left")
            == KeyBinding(KeyBinding.leftArrow, [.command, .option]))
        #expect(KeyBinding(parsing: "ctrl+alt+delete")?.modifiers == [.control, .option])
        #expect(KeyBinding(parsing: "escape") == KeyBinding(KeyBinding.escapeKey))
        #expect(KeyBinding(parsing: "cmd+return") == KeyBinding(KeyBinding.returnKey, .command))
    }

    @Test("glyph form parses")
    func parsesGlyphForm() {
        #expect(KeyBinding(parsing: "⌘N") == KeyBinding("n", .command))
        #expect(KeyBinding(parsing: "⌘⇧N") == KeyBinding("n", [.command, .shift]))
        #expect(KeyBinding(parsing: "⌘⌥←") == KeyBinding(KeyBinding.leftArrow, [.command, .option]))
        #expect(KeyBinding(parsing: "⌃⌥⇧⌘K")?.modifiers == [.control, .option, .shift, .command])
        #expect(KeyBinding(parsing: "⎋") == KeyBinding(KeyBinding.escapeKey))
    }

    @Test("`+` is both a separator and a key")
    func parsesPlus() {
        #expect(KeyBinding(parsing: "cmd++") == KeyBinding("+", .command))
        #expect(KeyBinding(parsing: "⌘+") == KeyBinding("+", .command))
        #expect(KeyBinding(parsing: "cmd+-") == KeyBinding("-", .command))
        #expect(KeyBinding(parsing: "cmd+\\") == KeyBinding("\\", .command))
        #expect(KeyBinding(parsing: "cmd+,") == KeyBinding(",", .command))
    }

    @Test("letters are case-insensitive")
    func lettersFold() {
        #expect(KeyBinding("N", .command) == KeyBinding("n", .command))
        #expect(KeyBinding(parsing: "cmd+N") == KeyBinding(parsing: "⌘n"))
    }

    @Test("nonsense does not parse")
    func rejectsGarbage() {
        #expect(KeyBinding(parsing: "") == nil)
        #expect(KeyBinding(parsing: "   ") == nil)
        #expect(KeyBinding(parsing: "cmd+notakey") == nil)
    }

    @Test("every declared binding round-trips through its serialized form")
    func serializationRoundTrips() {
        for command in AppCommands.all {
            for binding in command.declaredKeys {
                let round = KeyBinding(parsing: binding.serialized)
                #expect(round == binding, "\(command.id): \(binding.serialized) → \(String(describing: round))")
                let glyphRound = KeyBinding(parsing: binding.display)
                #expect(glyphRound == binding, "\(command.id): \(binding.display) → \(String(describing: glyphRound))")
            }
        }
    }

    @Test("modifier glyphs render in Apple's order")
    func rendersGlyphs() {
        #expect(KeyBinding("k", [.command, .shift]).display == "⇧⌘K")
        #expect(KeyBinding("k", [.control, .option, .shift, .command]).display == "⌃⌥⇧⌘K")
        #expect(KeyBinding(KeyBinding.leftArrow, [.command, .option]).display == "⌥⌘←")
        #expect(KeyBinding(KeyBinding.returnKey, .command).display == "⌘⏎")
        #expect(KeyBinding(KeyBinding.escapeKey).display == "⎋")
    }

    @Test("bindings speak themselves for VoiceOver")
    func spokenDescription() {
        #expect(KeyBinding("n", [.command, .shift]).spokenDescription == "Shift Command N")
        #expect(KeyBinding(KeyBinding.leftArrow, .command).spokenDescription == "Command Left Arrow")
    }

    // MARK: - NSEvent matching

    @Test("an event matches only its exact modifier set")
    func matchingIsExact() throws {
        let plainG = try #require(KeyBinding(parsing: "cmd+g"))
        let shiftG = try #require(KeyBinding(parsing: "cmd+shift+g"))

        let event = try #require(Self.keyDown("g", [.command]))
        #expect(plainG.matches(event))
        #expect(!shiftG.matches(event), "⌘G must not fire ⌘⇧G")

        let shifted = try #require(Self.keyDown("G", [.command, .shift]))
        #expect(shiftG.matches(shifted))
        #expect(!plainG.matches(shifted), "⌘⇧G must not fire ⌘G")
    }

    @Test("a bare key with no modifiers matches")
    func matchesEscape() throws {
        let escape = KeyBinding(KeyBinding.escapeKey)
        let event = try #require(Self.keyDown("\u{1B}", []))
        #expect(escape.matches(event))

        let withCommand = try #require(Self.keyDown("\u{1B}", [.command]))
        #expect(!escape.matches(withCommand))
    }

    @Test("modifiers the app does not bind are ignored")
    func ignoresUnboundModifiers() throws {
        let binding = KeyBinding("n", .command)
        let event = try #require(Self.keyDown("n", [.command, .capsLock, .function]))
        #expect(binding.matches(event))
    }

    @Test("zoom in answers to ⌘=, ⌘⇧= and ⌘+")
    func zoomInAlternates() throws {
        let resolver = ShortcutResolver()
        let keys = resolver.keys(for: "view.textSizeIncrease")

        for (characters, flags) in [("=", NSEvent.ModifierFlags.command),
                                    ("+", [.command, .shift])] {
            let event = try #require(Self.keyDown(characters, flags))
            #expect(
                keys.contains { $0.matches(event) },
                "no binding matched \(characters) with \(flags)")
        }
    }

    /// A synthetic key-down. `charactersIgnoringModifiers` is what the matcher reads.
    static func keyDown(_ characters: String, _ flags: NSEvent.ModifierFlags) -> NSEvent? {
        NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0,
            windowNumber: 0, context: nil, characters: characters,
            charactersIgnoringModifiers: characters, isARepeat: false, keyCode: 0)
    }
}
