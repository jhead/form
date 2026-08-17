import SwiftUI

/// The small pill above the composer: scope (`Local`), workspace folder (`dev`), or an
/// icon-only affordance. 24 pt tall, 6 pt radius, 11 pt label, hairline border (spec 08 §1).
public struct Chip: View {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled

    private let title: String?
    private let systemImage: String?
    private let trailingSystemImage: String?
    private let tone: FormTone
    private let isSelected: Bool
    private let tooltip: String?
    private let action: (() -> Void)?

    @State private var isHovering = false

    public init(
        _ title: String? = nil,
        systemImage: String? = nil,
        trailingSystemImage: String? = nil,
        tone: FormTone = .neutral,
        isSelected: Bool = false,
        tooltip: String? = nil,
        action: (() -> Void)? = nil
    ) {
        self.title = title
        self.systemImage = systemImage
        self.trailingSystemImage = trailingSystemImage
        self.tone = tone
        self.isSelected = isSelected
        self.tooltip = tooltip
        self.action = action
    }

    public var body: some View {
        Group {
            if let action {
                Button(action: action) { content }
                    .buttonStyle(.plain)
            } else {
                content
            }
        }
        .onHover { isHovering = $0 && isEnabled && action != nil }
        .animation(theme.motion.animation(.fast), value: isHovering)
        .animation(theme.motion.animation(.fast), value: isSelected)
        .formTooltip(tooltip)
        .opacity(isEnabled ? 1 : 0.45)
    }

    private var content: some View {
        HStack(spacing: theme.metrics.spacing.xs) {
            if let systemImage {
                Image(systemName: systemImage)
                    .font(.system(size: theme.metrics.iconSmall - 2, weight: .medium))
            }
            if let title {
                Text(title).typeStyle(theme.typography.micro)
            }
            if let trailingSystemImage {
                Image(systemName: trailingSystemImage)
                    .font(.system(size: theme.metrics.iconSmall - 3, weight: .semibold))
                    .foregroundStyle(theme.color.textTertiary)
            }
        }
        .foregroundStyle(foreground)
        .padding(.horizontal, title == nil ? theme.metrics.spacing.sm : theme.metrics.spacing.md)
        .frame(height: theme.metrics.chipHeight)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .strokeBorder(borderColor, lineWidth: theme.metrics.hairline * 2)
        )
        .contentShape(Rectangle())
    }

    private var foreground: ThemeColor {
        if isSelected { return tone == .neutral ? theme.color.textPrimary : tone.foreground(theme.color) }
        if isHovering { return theme.color.textPrimary }
        return tone == .neutral ? theme.color.textSecondary : tone.foreground(theme.color)
    }

    private var background: ThemeColor {
        if isSelected { return tone == .neutral ? theme.color.surfaceSelected : tone.background(theme.color) }
        if isHovering { return theme.color.surfaceHover }
        return theme.color.surface.opacity(0)
    }

    private var borderColor: ThemeColor {
        isSelected ? theme.color.borderStrong : theme.color.border
    }
}

#Preview("Chip") {
    ThemePreview {
        HStack(spacing: 6) {
            Chip("Local", systemImage: "laptopcomputer")
            Chip("dev", systemImage: "folder", tooltip: "/Users/jhead/dev/form") {}
            Chip(systemImage: "folder.badge.plus", tooltip: "Choose folder…") {}
            Chip("Unconfined", tone: .warning)
        }
        HStack(spacing: 6) {
            Chip("7d", isSelected: true) {}
            Chip("30d") {}
            Chip("All") {}
            Chip("main.swift", systemImage: "doc.text", trailingSystemImage: "xmark") {}
        }
    }
}
