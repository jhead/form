import SwiftUI

/// Renders a primitive side by side in both themes. Every `#Preview` in this module uses it,
/// so "does this survive a theme switch" is answered without leaving the canvas.
public struct ThemePreview<Content: View>: View {
    private let content: () -> Content
    private let padding: CGFloat

    public init(padding: CGFloat = 20, @ViewBuilder content: @escaping () -> Content) {
        self.content = content
        self.padding = padding
    }

    public var body: some View {
        HStack(spacing: 0) {
            pane(.light)
            pane(.dark)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    private func pane(_ theme: Theme) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(theme.id)
                .font(theme.typography.micro.font)
                .foregroundStyle(theme.color.textTertiary)
            content()
        }
        .padding(padding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.color.background)
        .theme(theme)
    }
}

/// Themed text for previews, so a preview never reaches for a raw font or color.
struct PreviewLabel: View {
    @Environment(\.theme) private var theme
    let text: String
    var style: KeyPath<TypeTokens, TypeStyle> = \.ui

    init(_ text: String, style: KeyPath<TypeTokens, TypeStyle> = \.ui) {
        self.text = text
        self.style = style
    }

    var body: some View {
        Text(text)
            .typeStyle(theme.typography[keyPath: style])
            .foregroundStyle(theme.color.textPrimary)
    }
}

/// Sample text and values used across previews, so a change to one primitive's preview does
/// not silently change what another is demonstrating.
enum PreviewFixture {
    static let title = "Add a health check endpoint"
    static let body = "Ran 5 commands, used a tool"
    static let paragraph = """
    The composer autogrows to twelve lines and then scrolls. Return sends, shift-return \
    inserts a newline.
    """
}
