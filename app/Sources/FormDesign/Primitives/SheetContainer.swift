import SwiftUI

/// The modal frame: a title bar, a scrolling body, and a trailing button rail. Preferences
/// (720 × 520) and every confirm dialog use it, so modals do not each invent a layout.
public struct SheetContainer<Content: View, Footer: View>: View {
    @Environment(\.theme) private var theme

    private let title: String
    private let subtitle: String?
    private let width: CGFloat?
    private let height: CGFloat?
    private let onClose: (() -> Void)?
    private let content: Content
    private let footer: Footer

    public init(
        title: String,
        subtitle: String? = nil,
        width: CGFloat? = nil,
        height: CGFloat? = nil,
        onClose: (() -> Void)? = nil,
        @ViewBuilder content: () -> Content,
        @ViewBuilder footer: () -> Footer
    ) {
        self.title = title
        self.subtitle = subtitle
        self.width = width
        self.height = height
        self.onClose = onClose
        self.content = content()
        self.footer = footer()
    }

    public var body: some View {
        VStack(spacing: 0) {
            header
            FormDivider()
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            if !(Footer.self == EmptyView.self) {
                FormDivider()
                HStack(spacing: theme.metrics.spacing.md) {
                    Spacer()
                    footer
                }
                .padding(.horizontal, theme.metrics.spacing.xl)
                .padding(.vertical, theme.metrics.spacing.lg)
            }
        }
        .frame(
            width: width ?? theme.metrics.sheetWidth,
            height: height ?? theme.metrics.sheetHeight
        )
        .background(theme.color.background)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous))
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: theme.metrics.spacing.md) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                Text(title)
                    .typeStyle(theme.typography.heading)
                    .foregroundStyle(theme.color.textPrimary)
                if let subtitle {
                    Text(subtitle)
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textTertiary)
                }
            }
            Spacer(minLength: theme.metrics.spacing.lg)
            if let onClose {
                IconButton(systemImage: "xmark", accessibilityLabel: "Close", size: .small, action: onClose)
                    .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 6 }
            }
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.vertical, theme.metrics.spacing.lg)
    }
}

public extension SheetContainer where Footer == EmptyView {
    init(
        title: String,
        subtitle: String? = nil,
        width: CGFloat? = nil,
        height: CGFloat? = nil,
        onClose: (() -> Void)? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title, subtitle: subtitle, width: width, height: height,
            onClose: onClose, content: content, footer: { EmptyView() }
        )
    }
}

/// Dims whatever is behind a modal, in `color.overlay`.
public struct SheetScrim: View {
    @Environment(\.theme) private var theme
    private let onTap: (() -> Void)?

    public init(onTap: (() -> Void)? = nil) {
        self.onTap = onTap
    }

    public var body: some View {
        theme.color.overlay
            .ignoresSafeArea()
            .contentShape(Rectangle())
            .onTapGesture { onTap?() }
            .accessibilityHidden(true)
    }
}

#Preview("SheetContainer") {
    ThemePreview(padding: 0) {
        SheetSample()
    }
}

private struct SheetSample: View {
    @Environment(\.theme) private var theme

    var body: some View {
        SheetContainer(
            title: "Preferences",
            subtitle: "Changes apply immediately",
            width: 420,
            height: 260,
            onClose: {}
        ) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
                PopoverRow("Startup view", value: "Home")
                PopoverRow("Confirm before delete", value: "On")
                PopoverRow("Auto-title sessions", value: "On")
            }
            .padding(theme.metrics.spacing.xl)
        } footer: {
            FormButton("Cancel", kind: .ghost) {}
            FormButton("Save", kind: .primary) {}
        }
    }
}
