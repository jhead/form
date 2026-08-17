import AppKit
import FormCore
import SwiftUI

/// Resolves `AppCommands.all` plus the user's `settings.shortcuts` overrides into the key
/// equivalents the app actually answers to.
///
/// ## The resolution rule
///
/// An explicit override always wins. If it collides with another command's default, that
/// default is dropped rather than both firing — which is what makes "no duplicate effective
/// bindings after user overrides" true *by construction* instead of by hoping the user picks
/// carefully. What was dropped is reported through `displacedKeys(for:)` so the Shortcuts
/// preferences tab can flag the conflict inline (spec 13) instead of silently eating a key.
///
/// Overrides are read from and written to `settings.shortcuts` — action id → key equivalent
/// (spec 04). Both `"cmd+shift+n"` and `"⌘⇧N"` parse; the canonical written form is the
/// former. An empty string unbinds the command.
@MainActor
@Observable
public final class ShortcutResolver {
    public private(set) var overrides: [String: String] = [:]

    /// Command id → the equivalents that command responds to, primary first.
    public private(set) var effectiveKeys: [String: [KeyBinding]] = [:]
    /// Command id → equivalents it would have had but lost to an override.
    public private(set) var displaced: [String: [KeyBinding]] = [:]

    @ObservationIgnored private var owner: [KeyBinding: String] = [:]
    @ObservationIgnored private let commands: [AppCommand]

    public init(commands: [AppCommand] = AppCommands.all, overrides: [String: String] = [:]) {
        self.commands = commands
        self.overrides = overrides
        recompute()
    }

    /// Called whenever `settings.shortcuts` changes. Cheap and idempotent.
    public func apply(overrides: [String: String]?) {
        let next = overrides ?? [:]
        guard next != self.overrides else { return }
        self.overrides = next
        recompute()
    }

    // MARK: - Lookup

    /// What the menu item shows. `nil` when the command is unbound.
    public func primaryKey(for id: String) -> KeyBinding? {
        effectiveKeys[id]?.first
    }

    public func primaryKey(for command: AppCommand) -> KeyBinding? {
        primaryKey(for: command.id)
    }

    public func keys(for id: String) -> [KeyBinding] {
        effectiveKeys[id] ?? []
    }

    public func displacedKeys(for id: String) -> [KeyBinding] {
        displaced[id] ?? []
    }

    /// Whether this command's binding came from the user rather than the table.
    public func isOverridden(_ id: String) -> Bool {
        overrides[id] != nil
    }

    /// The command an event fires, or `nil` when the app does not bind that key.
    public func command(for event: NSEvent) -> AppCommand? {
        guard event.type == .keyDown else { return nil }
        for command in commands {
            for binding in keys(for: command.id) where binding.matches(event) {
                return command
            }
        }
        return nil
    }

    /// Which command owns a candidate equivalent — what the Shortcuts tab asks before it
    /// lets the user record one.
    public func command(bound to binding: KeyBinding) -> AppCommand? {
        owner[binding].flatMap { id in commands.first { $0.id == id } }
    }

    /// Every effective equivalent in the app, for the conflict test and the cheat sheet.
    public var allEffectiveKeys: [KeyBinding] {
        commands.flatMap { keys(for: $0.id) }
    }

    // MARK: - Editing (the Shortcuts preferences tab)

    /// The settings patch W13 writes. `nil` clears the override and restores the default.
    public func settingsPatch(for id: String, binding: KeyBinding?) -> [String: String] {
        var next = overrides
        if let binding {
            next[id] = binding.serialized
        } else {
            next.removeValue(forKey: id)
        }
        return next
    }

    /// `settings.shortcuts` with every override cleared — "Reset to defaults".
    public var clearedOverrides: [String: String] { [:] }

    // MARK: - Resolution

    private func recompute() {
        var claimed: [KeyBinding: String] = [:]
        var resolved: [String: [KeyBinding]] = [:]
        var lost: [String: [KeyBinding]] = [:]

        // Pass 1: explicit overrides claim first, in table order so two overrides colliding
        // with each other resolve deterministically rather than by dictionary iteration.
        for command in commands {
            guard let raw = overrides[command.id] else { continue }
            let trimmed = raw.trimmingCharacters(in: .whitespaces)
            guard !trimmed.isEmpty else {
                resolved[command.id] = []  // deliberately unbound
                continue
            }
            guard let binding = KeyBinding(parsing: trimmed) else {
                Log.ui.error(
                    "unparseable shortcut override for \(command.id, privacy: .public): \(raw, privacy: .public)")
                continue
            }
            if let existing = claimed[binding], existing != command.id {
                lost[command.id, default: []].append(binding)
                resolved[command.id] = []
                continue
            }
            claimed[binding] = command.id
            resolved[command.id] = [binding]
        }

        // Pass 2: defaults fill in around them.
        for command in commands where resolved[command.id] == nil {
            var mine: [KeyBinding] = []
            for binding in command.declaredKeys {
                if let existing = claimed[binding], existing != command.id {
                    lost[command.id, default: []].append(binding)
                    continue
                }
                claimed[binding] = command.id
                mine.append(binding)
            }
            resolved[command.id] = mine
        }

        owner = claimed
        effectiveKeys = resolved
        displaced = lost
    }
}
