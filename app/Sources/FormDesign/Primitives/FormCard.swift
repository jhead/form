import SwiftUI

/// A dashboard panel: `surface` fill, `lg` radius, 1 pt `border`, 16 pt padding, a 12 pt
/// caption title and an optional 11 pt tertiary subtitle (spec 12 §1). W12's `ChartCard`
/// wraps this rather than re-deriving the numbers.
public struct FormCard<Content: View, Accessory: View>: View {
    @Environment(\.theme) private var theme

    private let title: String?
    private let subtitle: String?
    private let content: Content
    private let accessory: Accessory

    public init(
        title: String? = nil,
        subtitle: String? = nil,
        @ViewBuilder content: () -> Content,
        @ViewBuilder accessory: () -> Accessory
    ) {
        self.title = title
        self.subtitle = subtitle
        self.content = content()
        self.accessory = accessory()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            if title != nil || subtitle != nil {
                HStack(alignment: .firstTextBaseline, spacing: theme.metrics.spacing.md) {
                    VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                        if let title {
                            Text(title)
                                .typeStyle(theme.typography.caption)
                                .foregroundStyle(theme.color.textSecondary)
                        }
                        if let subtitle {
                            Text(subtitle)
                                .typeStyle(theme.typography.micro)
                                .foregroundStyle(theme.color.textTertiary)
                        }
                    }
                    Spacer(minLength: theme.metrics.spacing.md)
                    accessory
                }
            }
            content
        }
        .padding(theme.metrics.spacing.xl)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.surface)
        )
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
    }
}

public extension FormCard where Accessory == EmptyView {
    init(title: String? = nil, subtitle: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(title: title, subtitle: subtitle, content: content, accessory: { EmptyView() })
    }
}

public extension View {
    /// The window background behind the content pane.
    func contentBackground() -> some View {
        modifier(SurfaceBackground(kind: .content))
    }

    /// The sidebar's `.ultraThin` material over the window background — subtly lighter than
    /// the content pane in light mode, subtly darker in dark (spec 08 §1).
    func sidebarBackground() -> some View {
        modifier(SurfaceBackground(kind: .sidebar))
    }
}

private struct SurfaceBackground: ViewModifier {
    enum Kind { case content, sidebar }
    let kind: Kind

    @Environment(\.theme) private var theme

    func body(content: Content) -> some View {
        switch kind {
        case .content:
            content.background(theme.color.background)
        case .sidebar:
            content
                .background(theme.color.backgroundSidebar)
                .background(.ultraThinMaterial)
        }
    }
}

#Preview("FormCard") {
    ThemePreview {
        FormCardSamples()
    }
    .frame(width: 800)
}

private struct FormCardSamples: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(spacing: 16) {
            FormCard(title: "Tokens over time", subtitle: "Last 30 days") {
                HStack(alignment: .bottom, spacing: 4) {
                    ForEach(0 ..< 14, id: \.self) { index in
                        RoundedRectangle(cornerRadius: theme.metrics.radius.sm, style: .continuous)
                            .fill(theme.color.series(index % 4))
                            .frame(height: CGFloat(12 + (index * 7) % 46))
                    }
                }
                .frame(height: 60)
            } accessory: {
                Badge("21.8M", tone: .accent)
            }

            FormCard(title: "Cost") {
                EmptyState(
                    systemImage: "dollarsign.circle",
                    title: "No spend in this range",
                    message: "Cost appears once a run reports usage.",
                    isCompact: true
                )
            }
        }
    }
}
