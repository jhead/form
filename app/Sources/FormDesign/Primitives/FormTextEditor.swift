import SwiftUI

/// The composer's field: autogrows from `minLines` to `maxLines`, then scrolls internally
/// (F1.8). Height is measured off a hidden `Text` laid out with the same style and width, so
/// growth is exact rather than a per-character estimate.
public struct FormTextEditor: View {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled

    @Binding private var text: String
    private let placeholder: String
    private let minLines: Int
    private let maxLines: Int?
    private let style: TypeStyle?
    private let onSubmit: (() -> Void)?

    @FocusState private var isFocused: Bool
    @State private var measuredHeight: CGFloat = 0

    public init(
        text: Binding<String>,
        placeholder: String = "",
        minLines: Int = 1,
        maxLines: Int? = nil,
        style: TypeStyle? = nil,
        onSubmit: (() -> Void)? = nil
    ) {
        _text = text
        self.placeholder = placeholder
        self.minLines = minLines
        self.maxLines = maxLines
        self.style = style
        self.onSubmit = onSubmit
    }

    public var body: some View {
        let style = style ?? theme.typography.body
        let lineHeight = style.size * max(1.2, style.lineHeight)
        let inset = theme.metrics.spacing.lg
        let minHeight = lineHeight * CGFloat(minLines) + inset * 2
        let maxHeight = lineHeight * CGFloat(maxLines ?? theme.metrics.composerMaxLines) + inset * 2
        let height = min(max(measuredHeight + inset * 2, minHeight), maxHeight)

        ZStack(alignment: .topLeading) {
            measuringLayer(style: style, inset: inset)

            if text.isEmpty {
                Text(placeholder)
                    .typeStyle(style)
                    .foregroundStyle(theme.color.textTertiary)
                    .padding(.horizontal, inset)
                    .padding(.vertical, inset)
                    .allowsHitTesting(false)
            }

            TextEditor(text: $text)
                .typeStyle(style)
                .foregroundStyle(theme.color.textPrimary)
                .scrollContentBackground(.hidden)
                .scrollDisabled(measuredHeight + inset * 2 <= maxHeight)
                .padding(.horizontal, inset - 5) // TextEditor carries its own 5 pt text inset
                .padding(.vertical, inset)
                .focused($isFocused)
        }
        .frame(height: height)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                .fill(theme.color.surface)
        )
        .modifier(FocusRing(isFocused: isFocused, radius: theme.metrics.radius.xl))
        .opacity(isEnabled ? 1 : 0.5)
        .animation(theme.motion.animation(.fast), value: height)
        .animation(theme.motion.animation(.fast), value: isFocused)
        .onSubmit { onSubmit?() }
    }

    /// Invisible, non-interactive, and the only thing that knows how tall the text is.
    private func measuringLayer(style: TypeStyle, inset: CGFloat) -> some View {
        Text(text.isEmpty ? " " : text + "\n")
            .typeStyle(style)
            .padding(.horizontal, inset)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background {
                GeometryReader { proxy in
                    Color.clear
                        .onChange(of: proxy.size.height, initial: true) { _, height in
                            measuredHeight = height
                        }
                }
            }
            .hidden()
    }
}

#Preview("FormTextEditor") {
    FormTextEditorPreview()
}

private struct FormTextEditorPreview: View {
    @State private var short = ""
    @State private var long = PreviewFixture.paragraph

    var body: some View {
        ThemePreview {
            FormTextEditor(text: $short, placeholder: "Ask anything…")
            FormTextEditor(text: $long, placeholder: "Ask anything…", maxLines: 6)
        }
        .frame(width: 760)
    }
}
