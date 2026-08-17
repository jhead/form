import SwiftUI

/// A group header in the sidebar or a card header in the dashboard: 11 pt secondary medium,
/// 24 pt tall, disclosure chevron on hover, trailing actions (spec 08 §1).
public struct SectionHeader<Trailing: View>: View {
    @Environment(\.theme) private var theme

    private let title: String
    private let subtitle: String?
    private let isExpanded: Binding<Bool>?
    private let trailing: (Bool) -> Trailing

    @State private var isHovering = false

    public init(
        _ title: String,
        subtitle: String? = nil,
        isExpanded: Binding<Bool>? = nil,
        @ViewBuilder trailing: @escaping (Bool) -> Trailing
    ) {
        self.title = title
        self.subtitle = subtitle
        self.isExpanded = isExpanded
        self.trailing = trailing
    }

    public var body: some View {
        HStack(spacing: theme.metrics.spacing.xs) {
            if let isExpanded {
                Image(systemName: "chevron.right")
                    .font(.system(size: theme.metrics.iconSmall - 4, weight: .semibold))
                    .foregroundStyle(theme.color.textTertiary)
                    .rotationEffect(.degrees(isExpanded.wrappedValue ? 90 : 0))
                    .opacity(isHovering ? 1 : 0)
                    .frame(width: theme.metrics.spacing.lg)
                    .animation(theme.motion.animation(.fast), value: isExpanded.wrappedValue)
            }

            Text(title.uppercased())
                .typeStyle(theme.typography.micro.weighted(.medium))
                .foregroundStyle(theme.color.textSecondary)
                .lineLimit(1)

            if let subtitle {
                Text(subtitle)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .lineLimit(1)
            }

            Spacer(minLength: theme.metrics.spacing.md)

            trailing(isHovering)
        }
        .frame(height: theme.metrics.sectionHeaderHeight)
        .contentShape(Rectangle())
        .onHover { isHovering = $0 }
        .animation(theme.motion.animation(.fast), value: isHovering)
        .onTapGesture {
            guard let isExpanded else { return }
            isExpanded.wrappedValue.toggle()
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(title)
    }
}

public extension SectionHeader where Trailing == EmptyView {
    init(_ title: String, subtitle: String? = nil, isExpanded: Binding<Bool>? = nil) {
        self.init(title, subtitle: subtitle, isExpanded: isExpanded) { _ in EmptyView() }
    }
}

#Preview("SectionHeader") {
    SectionHeaderPreview()
}

private struct SectionHeaderPreview: View {
    @State private var expanded = true

    var body: some View {
        ThemePreview {
            SectionHeader("Ungrouped")
            SectionHeader("Work in progress", subtitle: "4", isExpanded: $expanded) { hovering in
                IconButton(systemImage: "ellipsis", accessibilityLabel: "Group actions", size: .small) {}
                    .opacity(hovering ? 1 : 0)
            }
        }
        .frame(width: 280)
    }
}
