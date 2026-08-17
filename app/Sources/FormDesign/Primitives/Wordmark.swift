import SwiftUI

/// The wordmark: `form`, always lowercase, always serif (PRD §1).
///
/// `Text(verbatim:)` rather than `Text(_:)` so no localisation table can ever capitalise it.
public struct Wordmark: View {
    @Environment(\.theme) private var theme

    private let size: CGFloat?
    private let weight: FontWeightToken

    /// `size` defaults to `typography.wordmark`, which already carries the user's text-size
    /// multiplier. Pass a size only when the layout demands a specific one.
    public init(size: CGFloat? = nil, weight: FontWeightToken = .regular) {
        self.size = size
        self.weight = weight
    }

    public var body: some View {
        Text(verbatim: "form")
            .font(style.font)
            .kerning(style.size * 0.005)
            .accessibilityLabel("form")
    }

    private var style: TypeStyle {
        var resolved = theme.typography.wordmark.weighted(weight)
        if let size { resolved.size = size }
        return resolved
    }
}

#Preview("Wordmark") {
    ThemePreview {
        WordmarkSamples()
    }
}

private struct WordmarkSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Wordmark()
                .foregroundStyle(theme.color.textPrimary)
            Wordmark(size: theme.typography.display.size)
                .foregroundStyle(theme.color.textPrimary)
            Wordmark(size: 44)
                .foregroundStyle(theme.color.accent)
            HStack(spacing: 8) {
                Wordmark(size: 13)
                    .foregroundStyle(theme.color.textSecondary)
                Text("·").foregroundStyle(theme.color.textTertiary)
                Text("Good evening")
                    .typeStyle(theme.typography.display)
                    .foregroundStyle(theme.color.textPrimary)
            }
        }
    }
}
