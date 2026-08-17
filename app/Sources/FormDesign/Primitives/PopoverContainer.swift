import SwiftUI

/// The chrome every popover shares: 10 pt radius, hairline border, strong shadow, 12 pt
/// padding, `.regular` material behind (spec 08 §1). Put this *inside* SwiftUI's
/// `.popover { }` so the content is themed even though the window is not.
public struct PopoverContainer<Content: View>: View {
    @Environment(\.theme) private var theme

    private let title: String?
    private let width: CGFloat?
    private let content: Content

    public init(title: String? = nil, width: CGFloat? = nil, @ViewBuilder content: () -> Content) {
        self.title = title
        self.width = width
        self.content = content()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            if let title {
                Text(title)
                    .typeStyle(theme.typography.micro.weighted(.medium))
                    .foregroundStyle(theme.color.textTertiary)
            }
            content
        }
        .padding(theme.metrics.popoverPadding)
        .frame(width: width ?? theme.metrics.popoverMaxWidth, alignment: .leading)
        .background(.regularMaterial)
        .background(theme.color.surface)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.popoverRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.popoverRadius, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
        .shadow(color: theme.color.overlay.color.opacity(0.5), radius: 18, y: 6)
    }
}

/// A label/value line: 12 pt secondary leading, 12 pt primary trailing, with an optional
/// 3 pt progress bar on a 10 % track underneath (spec 08 §1).
public struct PopoverRow: View {
    @Environment(\.theme) private var theme

    private let label: String
    private let value: String
    private let fraction: Double?
    private let tint: ThemeColor?

    public init(_ label: String, value: String, fraction: Double? = nil, tint: ThemeColor? = nil) {
        self.label = label
        self.value = value
        self.fraction = fraction
        self.tint = tint
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
            HStack(alignment: .firstTextBaseline) {
                Text(label)
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textSecondary)
                Spacer(minLength: theme.metrics.spacing.lg)
                Text(value)
                    .typeStyle(theme.typography.caption)
                    .tabularFigures()
                    .foregroundStyle(theme.color.textPrimary)
            }
            if let fraction {
                ProgressBar(value: fraction, tint: tint)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

#Preview("PopoverContainer") {
    ThemePreview {
        PopoverSample()
    }
}

private struct PopoverSample: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(spacing: 16) {
            PopoverContainer(title: "Context") {
                PopoverRow("System", value: "1,204", fraction: 0.02)
                PopoverRow("Tools", value: "8,310", fraction: 0.10, tint: theme.color.info)
                PopoverRow("Transcript", value: "42,880", fraction: 0.52, tint: theme.color.accent)
                PopoverRow("Attachments", value: "0", fraction: 0)
                PopoverRow("Output reserve", value: "8,192", fraction: 0.10, tint: theme.color.textTertiary)
                FormDivider()
                PopoverRow("Session tokens", value: "5.9k")
                PopoverRow("Cost", value: "$0.42")
            }
        }
    }
}
