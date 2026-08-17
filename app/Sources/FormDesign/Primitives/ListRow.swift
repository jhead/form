import SwiftUI

/// The interaction state a row's content may need — the sidebar swaps a rank number for a
/// status dot on hover, so the state has to reach the caller's builder.
public struct ListRowState: Sendable, Equatable {
    public var isHovering: Bool
    public var isSelected: Bool
    public var isPressed: Bool

    public init(isHovering: Bool = false, isSelected: Bool = false, isPressed: Bool = false) {
        self.isHovering = isHovering
        self.isSelected = isSelected
        self.isPressed = isPressed
    }
}

/// A full-width row with hover / selected / pressed fills. Sidebar sessions, nav rows,
/// palette results, leaderboard entries.
public struct ListRow<Content: View>: View {
    @Environment(\.theme) private var theme

    private let isSelected: Bool
    private let height: CGFloat?
    private let radius: CGFloat?
    private let horizontalInset: CGFloat?
    private let action: (() -> Void)?
    private let content: (ListRowState) -> Content

    @State private var isHovering = false
    @State private var isPressed = false

    public init(
        isSelected: Bool = false,
        height: CGFloat? = nil,
        radius: CGFloat? = nil,
        horizontalInset: CGFloat? = nil,
        action: (() -> Void)? = nil,
        @ViewBuilder content: @escaping (ListRowState) -> Content
    ) {
        self.isSelected = isSelected
        self.height = height
        self.radius = radius
        self.horizontalInset = horizontalInset
        self.action = action
        self.content = content
    }

    public var body: some View {
        let state = ListRowState(isHovering: isHovering, isSelected: isSelected, isPressed: isPressed)
        content(state)
            .padding(.horizontal, horizontalInset ?? theme.metrics.spacing.lg)
            .frame(height: height ?? theme.metrics.sidebarRowHeight)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: radius ?? theme.metrics.radius.lg, style: .continuous)
                    .fill(fill)
            )
            .contentShape(Rectangle())
            .onHover { isHovering = $0 }
            .simultaneousGesture(
                action == nil
                    ? nil
                    : DragGesture(minimumDistance: 0)
                    .onChanged { _ in isPressed = true }
                    .onEnded { _ in isPressed = false }
            )
            .onTapGesture { action?() }
            .animation(theme.motion.animation(.fast), value: isHovering)
            .animation(theme.motion.animation(.fast), value: isSelected)
            .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var fill: ThemeColor {
        if isSelected { return theme.color.surfaceSelected }
        if isPressed { return theme.color.surfaceSelected }
        if isHovering { return theme.color.surfaceHover }
        return theme.color.surfaceHover.opacity(0)
    }
}

#Preview("ListRow") {
    ThemePreview {
        VStack(spacing: 2) {
            ListRow(isSelected: true, action: {}) { state in
                RowSample(state: state, rank: 1, title: "Add a health check endpoint")
            }
            ListRow(action: {}) { state in
                RowSample(state: state, rank: 2, title: "Port the settings store to SQLite")
            }
            ListRow(action: {}) { state in
                RowSample(state: state, rank: 3, title: "A very long session title that has to truncate at the tail")
            }
        }
        .frame(width: 280)
    }
}

private struct RowSample: View {
    @Environment(\.theme) private var theme
    let state: ListRowState
    let rank: Int
    let title: String

    var body: some View {
        HStack(spacing: theme.metrics.spacing.md) {
            ZStack {
                if state.isHovering || state.isSelected {
                    PulsingDot(isActive: state.isSelected, tone: .accent)
                } else {
                    Text("\(rank)")
                        .typeStyle(theme.typography.micro)
                        .tabularFigures()
                        .foregroundStyle(theme.color.textTertiary)
                }
            }
            .frame(width: 16)

            Text(title)
                .typeStyle(theme.typography.ui)
                .lineLimit(1)
                .truncationMode(.tail)
                .foregroundStyle(state.isSelected ? theme.color.textPrimary : theme.color.textSecondary)
        }
    }
}
