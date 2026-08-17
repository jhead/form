import SwiftUI

/// A square, glyph-only button — sidebar controls, composer controls, header overflow.
/// `isActive` is the "this toggle is on" state (the sidebar toggle when the sidebar is open).
public struct IconButton: View {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled

    private let systemImage: String
    private let accessibilityLabel: String
    private let size: FormControlSize
    private let isActive: Bool
    private let tone: FormTone
    private let action: () -> Void

    @State private var isHovering = false

    public init(
        systemImage: String,
        accessibilityLabel: String,
        size: FormControlSize = .medium,
        isActive: Bool = false,
        tone: FormTone = .neutral,
        action: @escaping () -> Void
    ) {
        self.systemImage = systemImage
        self.accessibilityLabel = accessibilityLabel
        self.size = size
        self.isActive = isActive
        self.tone = tone
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: size.iconSize(theme.metrics), weight: .regular))
                .foregroundStyle(foreground)
                .frame(width: side, height: side)
                .background(
                    RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                        .fill(background)
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .opacity(isEnabled ? 1 : 0.4)
        .onHover { isHovering = $0 && isEnabled }
        .animation(theme.motion.animation(.fast), value: isHovering)
        .animation(theme.motion.animation(.fast), value: isActive)
        .accessibilityLabel(accessibilityLabel)
        .formTooltip(accessibilityLabel)
    }

    private var side: CGFloat {
        switch size {
        case .small: theme.metrics.controlHeightSmall
        case .medium: theme.metrics.iconButton
        case .large: theme.metrics.controlHeightLarge
        }
    }

    private var foreground: ThemeColor {
        if !isEnabled { return theme.color.textTertiary }
        if isActive { return tone == .neutral ? theme.color.textPrimary : tone.foreground(theme.color) }
        if isHovering { return theme.color.textPrimary }
        return tone == .neutral ? theme.color.textSecondary : tone.foreground(theme.color)
    }

    private var background: ThemeColor {
        if isActive { return theme.color.surfaceSelected }
        if isHovering { return theme.color.surfaceHover }
        return theme.color.surfaceHover.opacity(0)
    }
}

#Preview("IconButton") {
    ThemePreview {
        HStack(spacing: 4) {
            IconButton(systemImage: "sidebar.leading", accessibilityLabel: "Toggle sidebar") {}
            IconButton(systemImage: "magnifyingglass", accessibilityLabel: "Search") {}
            IconButton(systemImage: "sidebar.leading", accessibilityLabel: "Active", isActive: true) {}
            IconButton(systemImage: "trash", accessibilityLabel: "Delete", tone: .danger) {}
            IconButton(systemImage: "mic", accessibilityLabel: "Dictate") {}
                .disabled(true)
        }
        HStack(spacing: 4) {
            ForEach(FormControlSize.allCases, id: \.self) { size in
                IconButton(systemImage: "ellipsis", accessibilityLabel: "More", size: size) {}
            }
        }
    }
}
