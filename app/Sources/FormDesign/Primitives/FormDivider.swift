import SwiftUI

/// A hairline in `color.border`. Named `FormDivider` so it never shadows SwiftUI's
/// `Divider`, which picks its own color and would leak a non-token line into a themed
/// surface.
public struct FormDivider: View {
    @Environment(\.theme) private var theme

    private let axis: Axis
    private let inset: CGFloat
    private let color: ThemeColor?

    public init(_ axis: Axis = .horizontal, inset: CGFloat = 0, color: ThemeColor? = nil) {
        self.axis = axis
        self.inset = inset
        self.color = color
    }

    public var body: some View {
        Rectangle()
            .fill(color ?? theme.color.border)
            .frame(
                width: axis == .vertical ? theme.metrics.hairline * 2 : nil,
                height: axis == .horizontal ? theme.metrics.hairline * 2 : nil
            )
            .padding(axis == .horizontal ? .horizontal : .vertical, inset)
            .accessibilityHidden(true)
    }
}

#Preview("FormDivider") {
    ThemePreview {
        VStack(spacing: 12) {
            PreviewLabel("above")
            FormDivider()
            PreviewLabel("below")
            FormDivider(.horizontal, inset: 24)
            HStack(spacing: 12) {
                PreviewLabel("left")
                FormDivider(.vertical).frame(height: 16)
                PreviewLabel("right")
            }
        }
        .frame(width: 260)
    }
}
