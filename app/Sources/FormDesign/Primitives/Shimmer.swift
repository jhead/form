import SwiftUI

/// The thinking treatment (F6.3) and the dashboard's loading skeletons (spec 12 §4).
/// A travelling highlight masked to the content's own shape, so it works on text and on a
/// skeleton block alike. Inert under reduce-motion.
public struct ShimmerModifier: ViewModifier {
    @Environment(\.theme) private var theme

    let isActive: Bool
    let tint: ThemeColor?

    @State private var phase: CGFloat = -1

    public func body(content: Content) -> some View {
        content
            .overlay {
                if isActive {
                    GeometryReader { proxy in
                        let width = proxy.size.width
                        LinearGradient(
                            stops: [
                                .init(color: highlight.opacity(0), location: 0),
                                .init(color: highlight, location: 0.5),
                                .init(color: highlight.opacity(0), location: 1),
                            ],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                        .frame(width: max(width * 0.45, 60))
                        .offset(x: phase * (width + max(width * 0.45, 60)))
                        .blendMode(.plusLighter)
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
            }
            .onAppear(perform: start)
            .onChange(of: isActive) { _, _ in start() }
    }

    private var highlight: Color {
        (tint ?? theme.color.thinking).opacity(0.55).color
    }

    private func start() {
        phase = -1
        guard isActive, let animation = theme.motion.repeating(.pulse, curve: .linear, autoreverses: false) else {
            return
        }
        withAnimation(animation) { phase = 1 }
    }
}

public extension View {
    /// Sweeps a highlight across this view while `isActive`.
    func shimmer(_ isActive: Bool = true, tint: ThemeColor? = nil) -> some View {
        modifier(ShimmerModifier(isActive: isActive, tint: tint))
    }
}

/// A skeleton placeholder for loading states — a rounded block that shimmers on its own.
public struct ShimmerBlock: View {
    @Environment(\.theme) private var theme

    private let width: CGFloat?
    private let height: CGFloat
    private let radius: CGFloat?

    public init(width: CGFloat? = nil, height: CGFloat = 12, radius: CGFloat? = nil) {
        self.width = width
        self.height = height
        self.radius = radius
    }

    public var body: some View {
        RoundedRectangle(cornerRadius: radius ?? theme.metrics.radius.sm, style: .continuous)
            .fill(theme.color.textPrimary.opacity(0.08))
            .frame(width: width, height: height)
            .shimmer(tint: theme.color.textPrimary)
            .accessibilityHidden(true)
    }
}

#Preview("Shimmer") {
    ThemePreview {
        ShimmerSamples()
    }
    .frame(width: 560)
}

private struct ShimmerSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Thinking about the failing test…")
                .typeStyle(theme.typography.body)
                .foregroundStyle(theme.color.thinking)
                .shimmer()

            VStack(alignment: .leading, spacing: 8) {
                ShimmerBlock(width: 120, height: 10)
                ShimmerBlock(height: 10)
                ShimmerBlock(width: 200, height: 10)
                ShimmerBlock(height: 80, radius: theme.metrics.radius.lg)
            }
        }
    }
}
