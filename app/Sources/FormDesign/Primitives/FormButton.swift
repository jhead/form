import SwiftUI

public enum FormButtonKind: String, Sendable, CaseIterable {
    case primary, secondary, ghost, destructive
}

/// The app's button. Four kinds × three sizes; nothing else sets a fill or a radius.
public struct FormButton<Label: View>: View {
    private let kind: FormButtonKind
    private let size: FormControlSize
    private let fillsWidth: Bool
    private let action: () -> Void
    private let label: Label

    public init(
        kind: FormButtonKind = .secondary,
        size: FormControlSize = .medium,
        fillsWidth: Bool = false,
        action: @escaping () -> Void,
        @ViewBuilder label: () -> Label
    ) {
        self.kind = kind
        self.size = size
        self.fillsWidth = fillsWidth
        self.action = action
        self.label = label()
    }

    public var body: some View {
        Button(action: action) { label }
            .buttonStyle(FormButtonStyle(kind: kind, size: size, fillsWidth: fillsWidth))
    }
}

public extension FormButton where Label == FormButtonLabel {
    /// The common case: a title and an optional SF Symbol.
    init(
        _ title: String,
        systemImage: String? = nil,
        kind: FormButtonKind = .secondary,
        size: FormControlSize = .medium,
        fillsWidth: Bool = false,
        action: @escaping () -> Void
    ) {
        self.init(kind: kind, size: size, fillsWidth: fillsWidth, action: action) {
            FormButtonLabel(title: title, systemImage: systemImage, size: size)
        }
    }
}

public struct FormButtonLabel: View {
    @Environment(\.theme) private var theme
    let title: String
    let systemImage: String?
    let size: FormControlSize

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            if let systemImage {
                Image(systemName: systemImage)
                    .font(.system(size: size.iconSize(theme.metrics) - 2, weight: .medium))
            }
            Text(title)
        }
    }
}

// MARK: - Style

public struct FormButtonStyle: ButtonStyle {
    let kind: FormButtonKind
    let size: FormControlSize
    var fillsWidth: Bool = false

    public init(kind: FormButtonKind, size: FormControlSize = .medium, fillsWidth: Bool = false) {
        self.kind = kind
        self.size = size
        self.fillsWidth = fillsWidth
    }

    public func makeBody(configuration: Configuration) -> some View {
        StyledLabel(configuration: configuration, kind: kind, size: size, fillsWidth: fillsWidth)
    }

    private struct StyledLabel: View {
        let configuration: Configuration
        let kind: FormButtonKind
        let size: FormControlSize
        let fillsWidth: Bool

        @Environment(\.theme) private var theme
        @Environment(\.isEnabled) private var isEnabled
        @State private var isHovering = false

        var body: some View {
            configuration.label
                .typeStyle(size.typeStyle(theme.typography))
                .foregroundStyle(foreground)
                .padding(.horizontal, size.horizontalPadding(theme.metrics))
                .frame(maxWidth: fillsWidth ? .infinity : nil)
                .frame(height: size.height(theme.metrics))
                .background(
                    RoundedRectangle(cornerRadius: size.radius(theme.metrics), style: .continuous)
                        .fill(background)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: size.radius(theme.metrics), style: .continuous)
                        .strokeBorder(border, lineWidth: theme.metrics.hairline * 2)
                )
                .opacity(isEnabled ? 1 : 0.45)
                .contentShape(Rectangle())
                .onHover { isHovering = $0 && isEnabled }
                .animation(theme.motion.animation(.fast), value: isHovering)
                .animation(theme.motion.animation(.fast), value: configuration.isPressed)
        }

        private var isPressed: Bool { configuration.isPressed }

        private var foreground: ThemeColor {
            switch kind {
            case .primary, .destructive: theme.color.textInverted
            case .secondary: theme.color.textPrimary
            case .ghost: isHovering ? theme.color.textPrimary : theme.color.textSecondary
            }
        }

        private var background: ThemeColor {
            switch kind {
            case .primary:
                if isPressed { return theme.color.accentHover }
                return isHovering ? theme.color.accentHover : theme.color.accent
            case .destructive:
                let base = theme.color.danger
                return isPressed || isHovering ? base.mixed(with: theme.color.textPrimary, amount: 0.12) : base
            case .secondary:
                if isPressed { return theme.color.surfaceSelected }
                return isHovering ? theme.color.surfaceHover : theme.color.surfaceRaised
            case .ghost:
                if isPressed { return theme.color.surfaceSelected }
                return isHovering ? theme.color.surfaceHover : theme.color.surfaceHover.opacity(0)
            }
        }

        private var border: ThemeColor {
            switch kind {
            case .primary, .destructive, .ghost: theme.color.border.opacity(0)
            case .secondary: theme.color.border
            }
        }
    }
}

public extension ButtonStyle where Self == FormButtonStyle {
    static func form(_ kind: FormButtonKind, size: FormControlSize = .medium) -> FormButtonStyle {
        FormButtonStyle(kind: kind, size: size)
    }
}

#Preview("FormButton") {
    ThemePreview {
        ForEach(FormControlSize.allCases, id: \.self) { size in
            HStack(spacing: 8) {
                FormButton("New chat", systemImage: "plus", kind: .primary, size: size) {}
                FormButton("Cancel", kind: .secondary, size: size) {}
                FormButton("Skip", kind: .ghost, size: size) {}
                FormButton("Delete", kind: .destructive, size: size) {}
            }
        }
        FormButton("Disabled", kind: .primary) {}
            .disabled(true)
        FormButton("Full width", kind: .secondary, fillsWidth: true) {}
    }
}
