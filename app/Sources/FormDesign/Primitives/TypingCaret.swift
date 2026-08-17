import SwiftUI

/// The block caret trailing streamed assistant text. Distinct from the thinking shimmer
/// (F6.3): this is a hard-edged bar that blinks, not a soft sweep. Under reduce-motion it
/// holds steady rather than blinking, which still marks the insertion point.
public struct TypingCaret: View {
    @Environment(\.theme) private var theme

    private let isActive: Bool
    private let height: CGFloat?
    private let tint: ThemeColor?

    @State private var isDim = false

    public init(isActive: Bool = true, height: CGFloat? = nil, tint: ThemeColor? = nil) {
        self.isActive = isActive
        self.height = height
        self.tint = tint
    }

    public var body: some View {
        RoundedRectangle(cornerRadius: theme.metrics.caretWidth / 2, style: .continuous)
            .fill(tint ?? theme.color.accent)
            .frame(width: theme.metrics.caretWidth, height: height ?? theme.typography.body.size)
            .opacity(isActive ? (isDim ? 0.15 : 1) : 0)
            .onAppear(perform: start)
            .onChange(of: isActive) { _, _ in start() }
            .accessibilityHidden(true)
    }

    private func start() {
        isDim = false
        guard isActive, let animation = theme.motion.repeating(.slow, curve: .standard, autoreverses: true) else {
            return
        }
        withAnimation(animation) { isDim = true }
    }
}

#Preview("TypingCaret") {
    ThemePreview {
        TypingCaretSamples()
    }
}

private struct TypingCaretSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 2) {
                Text("Adding the health check endpoint")
                    .typeStyle(theme.typography.body)
                    .foregroundStyle(theme.color.textPrimary)
                TypingCaret()
            }
            HStack(spacing: 12) {
                TypingCaret(height: 24)
                TypingCaret(isActive: false, height: 24)
                TypingCaret(height: 24, tint: theme.color.thinking)
            }
        }
    }
}
