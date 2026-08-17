import SwiftUI

/// The session-row and transcript streaming indicator (F6.1). A 6 pt dot with a halo that
/// breathes while active. Under reduce-motion the halo is dropped and the dot stays solid,
/// so "streaming" is still legible without movement.
public struct PulsingDot: View {
    @Environment(\.theme) private var theme

    private let isActive: Bool
    private let tone: FormTone
    private let size: CGFloat?
    private let tint: ThemeColor?

    @State private var isPulsing = false

    public init(isActive: Bool = true, tone: FormTone = .accent, size: CGFloat? = nil, tint: ThemeColor? = nil) {
        self.isActive = isActive
        self.tone = tone
        self.size = size
        self.tint = tint
    }

    public var body: some View {
        let diameter = size ?? theme.metrics.statusDot
        ZStack {
            if isActive, !theme.motion.isReduced {
                Circle()
                    .fill(color.opacity(0.35))
                    .frame(width: diameter * 2.6, height: diameter * 2.6)
                    .scaleEffect(isPulsing ? 1.0 : 0.45)
                    .opacity(isPulsing ? 0 : 0.9)
            }
            Circle()
                .fill(isActive ? color : color.opacity(0.45))
                .frame(width: diameter, height: diameter)
        }
        .frame(width: diameter, height: diameter)
        .onAppear(perform: start)
        .onChange(of: isActive) { _, _ in start() }
        .accessibilityHidden(true)
    }

    private var color: ThemeColor {
        if let tint { return tint }
        return tone == .accent ? theme.color.streaming : tone.foreground(theme.color)
    }

    private func start() {
        isPulsing = false
        guard isActive, let animation = theme.motion.repeating(.pulse, curve: .standard, autoreverses: false) else {
            return
        }
        withAnimation(animation) { isPulsing = true }
    }
}

#Preview("PulsingDot") {
    ThemePreview {
        PulsingDotSamples()
    }
}

private struct PulsingDotSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 24) {
                PulsingDot()
                PulsingDot(tone: .success)
                PulsingDot(tone: .danger)
                PulsingDot(isActive: false, tone: .neutral)
            }
            HStack(spacing: 24) {
                PulsingDot(size: 10)
                PulsingDot(size: 14, tint: theme.color.thinking)
            }
        }
        .padding(.vertical, 8)
    }
}
