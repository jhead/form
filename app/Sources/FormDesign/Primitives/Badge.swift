import SwiftUI

/// A small status pill — capability flags on a model row, `hasKey` on a provider, a tool
/// call's outcome. Not interactive; use `Chip` when it needs to be pressed.
public struct Badge: View {
    @Environment(\.theme) private var theme

    private let title: String
    private let systemImage: String?
    private let tone: FormTone
    private let isFilled: Bool

    public init(_ title: String, systemImage: String? = nil, tone: FormTone = .neutral, isFilled: Bool = false) {
        self.title = title
        self.systemImage = systemImage
        self.tone = tone
        self.isFilled = isFilled
    }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.xs) {
            if let systemImage {
                Image(systemName: systemImage)
                    .font(.system(size: theme.metrics.iconSmall - 4, weight: .semibold))
            }
            Text(title).typeStyle(theme.typography.micro)
        }
        .foregroundStyle(foreground)
        .padding(.horizontal, theme.metrics.spacing.sm)
        .padding(.vertical, theme.metrics.spacing.xxs)
        .background(
            Capsule(style: .continuous).fill(background)
        )
        .overlay(
            Capsule(style: .continuous)
                .strokeBorder(isFilled ? background : tone.foreground(theme.color).opacity(0.28),
                              lineWidth: theme.metrics.hairline * 2)
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel(title)
    }

    private var foreground: ThemeColor {
        isFilled ? theme.color.textInverted : tone.foreground(theme.color)
    }

    private var background: ThemeColor {
        isFilled ? tone.foreground(theme.color) : tone.background(theme.color)
    }
}

#Preview("Badge") {
    ThemePreview {
        HStack(spacing: 6) {
            ForEach(FormTone.allCases, id: \.self) { tone in
                Badge(tone.rawValue, systemImage: tone.systemImage, tone: tone)
            }
        }
        HStack(spacing: 6) {
            ForEach(FormTone.allCases, id: \.self) { tone in
                Badge(tone.rawValue, tone: tone, isFilled: true)
            }
        }
        HStack(spacing: 6) {
            Badge("vision")
            Badge("tools")
            Badge("reasoning")
            Badge("caching")
            Badge("key set", systemImage: "checkmark", tone: .success)
        }
    }
}
