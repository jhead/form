import FormDesign
import SwiftUI

/// Renders a key equivalent the way a Mac menu does — `⌃⌥⇧⌘` in Apple's order, arrows and
/// `⏎`/`⎋` as glyphs rather than words (spec 14 §5).
public struct KeyCapView: View {
    @Environment(\.theme) private var theme

    private let binding: KeyBinding
    private let isProminent: Bool

    public init(binding: KeyBinding, isProminent: Bool = false) {
        self.binding = binding
        self.isProminent = isProminent
    }

    public var body: some View {
        Text(binding.display)
            .typeStyle(theme.typography.micro.weighted(.medium))
            .foregroundStyle(isProminent ? theme.color.textPrimary : theme.color.textSecondary)
            .padding(.horizontal, theme.metrics.spacing.sm)
            .padding(.vertical, theme.metrics.spacing.xxs)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                    .fill(theme.color.surfaceRaised)
            )
            .overlay(
                RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                    .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
            )
            .accessibilityLabel(binding.spokenDescription)
    }
}

/// The primary equivalent plus any aliases, e.g. `⌘[  ⌘⌥←`.
public struct KeyCapRow: View {
    @Environment(\.theme) private var theme

    private let bindings: [KeyBinding]

    public init(_ bindings: [KeyBinding]) {
        self.bindings = bindings
    }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.xs) {
            ForEach(Array(bindings.enumerated()), id: \.offset) { index, binding in
                KeyCapView(binding: binding, isProminent: index == 0)
            }
        }
    }
}

#Preview("Key caps") {
    ThemePreview {
        KeyCapRow([KeyBinding("n", [.command, .shift])])
        KeyCapRow([KeyBinding("[", .command), KeyBinding(KeyBinding.leftArrow, [.command, .option])])
        KeyCapRow([KeyBinding(KeyBinding.escapeKey), KeyBinding(KeyBinding.returnKey, .command)])
    }
}
