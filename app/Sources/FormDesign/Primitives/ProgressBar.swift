import SwiftUI

/// A 3 pt fully-rounded bar on a 10 % track (spec 08 §1). `value: nil` is indeterminate — a
/// travelling segment for a tool call that has not reported progress yet (F6.2). Under
/// reduce-motion the indeterminate form settles into a static partial fill rather than
/// looping.
public struct ProgressBar: View {
    @Environment(\.theme) private var theme

    private let value: Double?
    private let tint: ThemeColor?
    private let height: CGFloat?

    @State private var phase: CGFloat = 0

    public init(value: Double?, tint: ThemeColor? = nil, height: CGFloat? = nil) {
        self.value = value
        self.tint = tint
        self.height = height
    }

    public var body: some View {
        let barHeight = height ?? theme.metrics.progressBarHeight
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule(style: .continuous)
                    .fill(theme.color.textPrimary.opacity(0.10))

                Capsule(style: .continuous)
                    .fill(tint ?? theme.color.accent)
                    .frame(width: fillWidth(in: proxy.size.width))
                    .offset(x: offset(in: proxy.size.width))
            }
        }
        .frame(height: barHeight)
        .animation(theme.motion.animation(.normal, curve: .emphasized), value: value)
        .onAppear(perform: startIndeterminate)
        .onChange(of: value == nil) { _, _ in startIndeterminate() }
        .accessibilityElement()
        .accessibilityLabel("Progress")
        .accessibilityValue(value.map { "\(Int(($0 * 100).rounded())) percent" } ?? "In progress")
    }

    private func fillWidth(in total: CGFloat) -> CGFloat {
        guard let value else { return total * 0.28 }
        return total * min(1, max(0, value))
    }

    private func offset(in total: CGFloat) -> CGFloat {
        guard value == nil else { return 0 }
        return phase * (total * 0.72)
    }

    private func startIndeterminate() {
        guard value == nil else {
            phase = 0
            return
        }
        phase = 0
        guard let animation = theme.motion.repeating(.pulse, curve: .standard, autoreverses: true) else {
            // Reduce-motion: leave the segment parked at the leading edge.
            return
        }
        withAnimation(animation) { phase = 1 }
    }
}

#Preview("ProgressBar") {
    ThemePreview {
        ProgressBarSamples()
    }
    .frame(width: 520)
}

private struct ProgressBarSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(spacing: 14) {
            ProgressBar(value: 0.0)
            ProgressBar(value: 0.35)
            ProgressBar(value: 0.78, tint: theme.color.success)
            ProgressBar(value: 1.0, tint: theme.color.success)
            ProgressBar(value: nil)
            ProgressBar(value: 0.5, height: 6)
        }
    }
}
