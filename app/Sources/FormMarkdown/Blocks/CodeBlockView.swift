import FormCore
import FormDesign
import SwiftUI

/// A fenced code block (F7.2): language label, hover copy button, syntax colors from the
/// theme, optional line numbers and soft wrap.
///
/// **The block scrolls; the page never does.** The horizontal `ScrollView` is the only thing
/// in this module allowed to be wider than the column, and it is clipped to the panel — a
/// 400-character line moves the code, not the transcript.
struct CodeBlockView: View {
    let language: String?
    let code: String
    let tokens: [CodeToken]
    let partial: Bool
    let metrics: MarkdownMetrics

    @State private var isHovering = false
    @State private var didCopy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            content
        }
        .background(
            RoundedRectangle(cornerRadius: metrics.radius, style: .continuous)
                .fill(metrics.theme.color.surfaceRaised)
        )
        .overlay(
            RoundedRectangle(cornerRadius: metrics.radius, style: .continuous)
                .strokeBorder(metrics.theme.color.border, lineWidth: metrics.hairline * 2)
        )
        .clipShape(RoundedRectangle(cornerRadius: metrics.radius, style: .continuous))
        .onHover { isHovering = $0 }
        // The checkmark holds for the theme's confirmation beat (1.2 s, `motion.pulse`).
        .task(id: didCopy) {
            guard didCopy else { return }
            try? await Task.sleep(for: .seconds(metrics.copyConfirmation))
            guard !Task.isCancelled else { return }
            didCopy = false
        }
    }

    /// Suppressed while the fence is still being written (spec 11 §4): copying half a code
    /// block is never what the user meant.
    var showsCopyButton: Bool { metrics.style.showsCopyButton && !partial }

    private var header: some View {
        HStack(spacing: metrics.theme.metrics.spacing.md) {
            if let language, !language.isEmpty {
                Text(language)
                    .typeStyle(metrics.theme.typography.micro)
                    .foregroundStyle(metrics.theme.color.textTertiary)
            }
            Spacer(minLength: 0)
            if showsCopyButton, isHovering || didCopy {
                IconButton(
                    systemImage: didCopy ? "checkmark" : "doc.on.doc",
                    accessibilityLabel: didCopy ? "Copied" : "Copy code",
                    size: .small,
                    tone: didCopy ? .success : .neutral
                ) { copy() }
            }
        }
        .frame(height: metrics.codeHeaderHeight)
        .padding(.horizontal, metrics.codePadding)
        .padding(.top, metrics.theme.metrics.spacing.xs)
    }

    private var content: some View {
        HStack(alignment: .top, spacing: metrics.theme.metrics.spacing.lg) {
            if metrics.style.showLineNumbers { gutter }
            if metrics.style.wrapCode {
                text
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                ScrollView(.horizontal) {
                    text
                        .padding(.trailing, metrics.codePadding)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding(.leading, metrics.codePadding)
        .padding(.bottom, metrics.codePadding)
        .padding(.top, metrics.theme.metrics.spacing.xs)
    }

    private var text: some View {
        Text(CodeHighlighting.attributed(code: code, tokens: tokens, metrics: metrics))
            .textSelection(.enabled)
            .lineSpacing(metrics.code.lineSpacing)
            .fixedSize(horizontal: !metrics.style.wrapCode, vertical: true)
            .frame(maxWidth: metrics.style.wrapCode ? .infinity : nil, alignment: .leading)
    }

    private var gutter: some View {
        VStack(alignment: .trailing, spacing: 0) {
            ForEach(1 ... max(1, CodeHighlighting.lineCount(of: code)), id: \.self) { line in
                Text("\(line)")
                    .typeStyle(metrics.code)
                    .lineSpacing(metrics.code.lineSpacing)
                    .tabularFigures()
                    .foregroundStyle(metrics.theme.color.textTertiary)
            }
        }
        // Outside the horizontal ScrollView on purpose: the numbers stay put while the code
        // moves under them.
        .fixedSize(horizontal: true, vertical: true)
    }

    private func copy() {
        // The source, not the rendered text — there is no difference for code, which is the
        // point: a code block is already markdown's own verbatim form.
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
        didCopy = true
    }
}

#Preview("code blocks — plain, numbered, wrapped") {
    ThemePreview {
        MarkdownView(doc: MarkdownFixture.codeOnly)
        MarkdownView(
            doc: MarkdownFixture.codeOnly, style: MarkdownStyle(showLineNumbers: true))
        MarkdownView(
            doc: MarkdownFixture.codeOnly,
            style: MarkdownStyle(showLineNumbers: true, wrapCode: true))
    }
    .frame(width: 820)
}

#Preview("code block — still streaming") {
    ThemePreview {
        MarkdownView(doc: MarkdownFixture.streamingTail)
    }
    .frame(width: 820)
}
