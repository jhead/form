import SwiftUI

/// A hover tooltip with the platform's dwell delay.
///
/// AppKit's own tooltip does not follow the theme and cannot show a second line, so this
/// draws its own — but `.help()` is applied as well, because that is what VoiceOver and the
/// accessibility inspector read.
public struct Tooltip: View {
    @Environment(\.theme) private var theme
    private let text: String
    private let detail: String?

    public init(_ text: String, detail: String? = nil) {
        self.text = text
        self.detail = detail
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
            Text(text)
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textPrimary)
            if let detail {
                Text(detail)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
            }
        }
        .padding(.horizontal, theme.metrics.spacing.md)
        .padding(.vertical, theme.metrics.spacing.sm)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(theme.color.surface)
                .overlay(
                    RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                        .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
                )
                .shadow(color: theme.color.overlay.color.opacity(0.35), radius: 8, y: 3)
        )
        .fixedSize()
    }
}

public extension View {
    /// Themed tooltip on hover, plus the system `help` string for accessibility.
    func formTooltip(_ text: String?, detail: String? = nil, edge: Edge = .bottom) -> some View {
        modifier(TooltipModifier(text: text, detail: detail, edge: edge))
    }
}

private struct TooltipModifier: ViewModifier {
    let text: String?
    let detail: String?
    let edge: Edge

    @Environment(\.theme) private var theme
    @State private var isHovering = false
    @State private var isVisible = false

    func body(content: Content) -> some View {
        content
            .help(text ?? "")
            .onHover { hovering in
                isHovering = hovering
                if !hovering { isVisible = false }
            }
            .task(id: isHovering) {
                guard isHovering, text != nil else { return }
                // The dwell delay macOS uses before showing a tooltip.
                try? await Task.sleep(for: .milliseconds(500))
                guard !Task.isCancelled, isHovering else { return }
                isVisible = true
            }
            .overlay(alignment: alignment) {
                if isVisible, let text {
                    Tooltip(text, detail: detail)
                        .fixedSize()
                        .offset(offset)
                        .transition(.opacity)
                        .allowsHitTesting(false)
                        .zIndex(1)
                }
            }
            .animation(theme.motion.animation(.fast), value: isVisible)
    }

    private var alignment: Alignment {
        switch edge {
        case .top: .top
        case .bottom: .bottom
        case .leading: .leading
        case .trailing: .trailing
        }
    }

    private var offset: CGSize {
        let gap = theme.metrics.spacing.sm
        switch edge {
        case .top: return CGSize(width: 0, height: -(theme.metrics.iconButton + gap))
        case .bottom: return CGSize(width: 0, height: theme.metrics.iconButton + gap)
        case .leading: return CGSize(width: -(theme.metrics.popoverMaxWidth / 3), height: 0)
        case .trailing: return CGSize(width: theme.metrics.popoverMaxWidth / 3, height: 0)
        }
    }
}

#Preview("Tooltip") {
    ThemePreview {
        Tooltip("Toggle sidebar", detail: "⌘\\")
        Tooltip("~/dev/form")
    }
}
