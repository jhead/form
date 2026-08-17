import FormDesign
import SwiftUI

/// A glyph that opens a menu, sized and coloured like `IconButton` but backed by `Menu` so
/// AppKit owns the popup. Used by group headers (`⋯`), the sidebar footer chevron and the
/// content header's `⋮` overflow.
struct SidebarMenuButton<Content: View>: View {
    @Environment(\.theme) private var theme

    let systemImage: String
    let accessibilityLabel: String
    var rotation: Angle = .zero
    @ViewBuilder let content: () -> Content

    @State private var isHovering = false

    var body: some View {
        Menu {
            content()
        } label: {
            Image(systemName: systemImage)
                .typeStyle(theme.typography.uiMedium)
                .rotationEffect(rotation)
                .foregroundStyle(isHovering ? theme.color.textPrimary : theme.color.textSecondary)
                .frame(width: theme.metrics.controlHeightSmall, height: theme.metrics.controlHeightSmall)
                .background(
                    RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                        .fill(isHovering ? theme.color.surfaceHover : theme.color.surfaceHover.opacity(0))
                )
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .onHover { isHovering = $0 }
        .animation(theme.motion.animation(.fast), value: isHovering)
        .accessibilityLabel(accessibilityLabel)
    }
}
