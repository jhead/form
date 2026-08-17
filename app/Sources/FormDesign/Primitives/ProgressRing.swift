import SwiftUI

/// The composer's context-usage ring (F10). Determinate, animates between values rather than
/// snapping (F6.4), and recolors at the thresholds (F10.2). Defaults are 75 % → `warning`
/// and 90 % → `danger`.
public struct ProgressRing: View {
    @Environment(\.theme) private var theme

    private let value: Double
    private let size: CGFloat?
    private let lineWidth: CGFloat?
    private let warningThreshold: Double
    private let dangerThreshold: Double
    private let tint: ThemeColor?
    private let showsPercentage: Bool

    public init(
        value: Double,
        size: CGFloat? = nil,
        lineWidth: CGFloat? = nil,
        warningThreshold: Double = 0.75,
        dangerThreshold: Double = 0.90,
        tint: ThemeColor? = nil,
        showsPercentage: Bool = false
    ) {
        self.value = value
        self.size = size
        self.lineWidth = lineWidth
        self.warningThreshold = warningThreshold
        self.dangerThreshold = dangerThreshold
        self.tint = tint
        self.showsPercentage = showsPercentage
    }

    public var body: some View {
        let diameter = size ?? theme.metrics.contextRing
        let stroke = lineWidth ?? theme.metrics.ringLineWidth
        let clamped = min(1, max(0, value))

        ZStack {
            Circle()
                .stroke(theme.color.textPrimary.opacity(0.10), lineWidth: stroke)

            Circle()
                .trim(from: 0, to: clamped)
                .stroke(color, style: StrokeStyle(lineWidth: stroke, lineCap: .round))
                .rotationEffect(.degrees(-90))

            if showsPercentage {
                Text("\(Int((clamped * 100).rounded()))")
                    .typeStyle(theme.typography.micro)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textSecondary)
            }
        }
        .frame(width: diameter, height: diameter)
        .animation(theme.motion.animation(.slow, curve: .emphasized), value: clamped)
        .animation(theme.motion.animation(.normal), value: color)
        .accessibilityElement()
        .accessibilityLabel("Context used")
        .accessibilityValue("\(Int((clamped * 100).rounded())) percent")
    }

    private var color: ThemeColor {
        if let tint { return tint }
        if value >= dangerThreshold { return theme.color.danger }
        if value >= warningThreshold { return theme.color.warning }
        return theme.color.accent
    }
}

#Preview("ProgressRing") {
    ProgressRingPreview()
}

private struct ProgressRingPreview: View {
    @State private var value = 0.32

    var body: some View {
        ThemePreview {
            HStack(spacing: 16) {
                ProgressRing(value: 0.18)
                ProgressRing(value: 0.62)
                ProgressRing(value: 0.80)
                ProgressRing(value: 0.95)
            }
            HStack(spacing: 16) {
                ProgressRing(value: 0.42, size: 40, lineWidth: 4, showsPercentage: true)
                ProgressRing(value: 0.78, size: 40, lineWidth: 4, showsPercentage: true)
                ProgressRing(value: 0.93, size: 40, lineWidth: 4, showsPercentage: true)
            }
            HStack(spacing: 12) {
                ProgressRing(value: value, size: 24, lineWidth: 3)
                Slider(value: $value)
            }
        }
        .frame(width: 560)
    }
}
