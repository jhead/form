import AppKit
import FormDesign
import SwiftUI

/// Records a key equivalent by capturing one key-down (spec 13, Shortcuts).
///
/// SwiftUI has no way to observe a raw key-down with modifiers on macOS 14, so the capture is
/// an `NSView` that becomes first responder while recording. It is deliberately *not* a global
/// monitor: a recorder that swallowed keys app-wide would eat the shortcuts of whatever is
/// behind the sheet.
///
/// The recorded value is a `KeyBinding` — W14's type, the same one the menu bar and the key
/// monitor use — so what is recorded here is exactly what the app will answer to.
/// `Esc` cancels, `⌫` clears the override and falls back to the default.
struct KeyRecorderField: View {
    @Environment(\.theme) private var theme

    let current: KeyBinding?
    let defaultKey: KeyBinding?
    let conflict: String?
    let onRecord: (KeyBinding) -> Void
    let onClear: () -> Void

    @State private var isRecording = false

    private var isOverridden: Bool { current != defaultKey }

    var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            Chip(
                label,
                systemImage: conflict == nil ? nil : "exclamationmark.triangle",
                tone: conflict == nil ? (isRecording ? .accent : .neutral) : .warning,
                isSelected: isRecording,
                tooltip: isRecording
                    ? "Press a key combination · Esc cancels · ⌫ clears"
                    : (conflict ?? "Click to record")
            ) {
                isRecording.toggle()
            }
            .background {
                if isRecording {
                    KeyCaptureView(
                        onCapture: { binding in
                            isRecording = false
                            onRecord(binding)
                        },
                        onCancel: { isRecording = false },
                        onClear: {
                            isRecording = false
                            onClear()
                        }
                    )
                    .allowsHitTesting(false)
                }
            }

            IconButton(
                systemImage: "arrow.uturn.backward",
                accessibilityLabel: "Reset to default",
                size: .small,
                action: onClear
            )
            .opacity(isOverridden ? 1 : 0)
            .disabled(!isOverridden)
        }
        .animation(theme.motion.animation(.fast), value: isRecording)
    }

    private var label: String {
        if isRecording { return "Press a key…" }
        return current?.display ?? "—"
    }
}

/// The first-responder shim. It exists only to be the responder while `isRecording` is true.
private struct KeyCaptureView: NSViewRepresentable {
    let onCapture: (KeyBinding) -> Void
    let onCancel: () -> Void
    let onClear: () -> Void

    func makeNSView(context: Context) -> CaptureView {
        CaptureView()
    }

    func updateNSView(_ view: CaptureView, context: Context) {
        view.onCapture = onCapture
        view.onCancel = onCancel
        view.onClear = onClear
        // Deferred: the view is not in a window yet on the pass that creates it.
        Task { @MainActor in view.window?.makeFirstResponder(view) }
    }

    final class CaptureView: NSView {
        var onCapture: ((KeyBinding) -> Void)?
        var onCancel: (() -> Void)?
        var onClear: (() -> Void)?

        override var acceptsFirstResponder: Bool { true }

        override func keyDown(with event: NSEvent) {
            switch event.keyCode {
            case 53:  // esc — cancel without changing anything
                onCancel?()
            case 51:  // delete — drop the override
                onClear?()
            default:
                guard let binding = Self.binding(from: event) else {
                    NSSound.beep()
                    return
                }
                onCapture?(binding)
            }
        }

        /// While recording, a key equivalent must be *captured* rather than executed —
        /// otherwise recording `⌘W` would archive the session instead.
        override func performKeyEquivalent(with event: NSEvent) -> Bool {
            keyDown(with: event)
            return true
        }

        /// `charactersIgnoringModifiers` is the unshifted character, which is what makes `⇧/`
        /// record as `⇧/` rather than as `?`.
        private static func binding(from event: NSEvent) -> KeyBinding? {
            let modifiers = KeyModifiers(event.modifierFlags)
            if let named = namedKey(for: event.keyCode) {
                return KeyBinding(named, modifiers)
            }
            guard let characters = event.charactersIgnoringModifiers,
                let character = characters.first,
                character.isLetter || character.isNumber || character.isPunctuation
                    || character.isSymbol
            else { return nil }
            return KeyBinding(character, modifiers)
        }

        private static func namedKey(for code: UInt16) -> Character? {
            switch code {
            case 123: KeyBinding.leftArrow
            case 124: KeyBinding.rightArrow
            case 126: KeyBinding.upArrow
            case 125: KeyBinding.downArrow
            case 36, 76: KeyBinding.returnKey
            case 48: "\t"
            case 49: " "
            default: nil
            }
        }
    }
}
