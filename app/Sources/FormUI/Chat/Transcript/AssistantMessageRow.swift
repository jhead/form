import SwiftUI
import FormCore
import FormDesign

/// A reply: no bubble, full column, markdown from the core (F1.2).
///
/// The row owns one `MarkdownStream` and feeds it this message's text. That is the only
/// place a delta turns into work: everything else in the row is a label. The stream is
/// debounced, so a 450-line response reparses tens of times, not hundreds (spec 10 §2).
struct AssistantMessageRow: View {
    @Environment(\.theme) private var theme

    let entry: Entry
    let message: AssistantMessage
    let isStreaming: Bool
    let effort: ThinkingLevel?
    let client: CoreClient
    let onRetry: () -> Void
    let onBranch: () -> Void

    @State private var markdown: MarkdownStream?
    @State private var isHovering = false

    private var thinking: String { message.thinking }
    private var text: String { message.text }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            if !thinking.isEmpty {
                ThinkingBlock(text: thinking, effort: effort, isStreaming: isStreaming)
            }

            if !text.isEmpty {
                if let markdown {
                    MarkdownDocView(doc: markdown.doc, showsCaret: isStreaming)
                }
            } else if isStreaming, thinking.isEmpty {
                // Between `message_start` and the first delta there is nothing to draw but
                // the fact that something is happening.
                HStack(spacing: theme.metrics.spacing.md) {
                    PulsingDot()
                    Text("Working…")
                        .typeStyle(theme.typography.caption)
                        .foregroundStyle(theme.color.textTertiary)
                }
            }

            // Trailing gutter, per spec 10 §3. Kept in flow rather than overlaid so nothing
            // the user is reading is covered, and so hovering does not reflow the column.
            HStack(spacing: 0) {
                Spacer(minLength: 0)
                MessageActions(
                    timestamp: message.timestamp,
                    isVisible: isHovering && !isStreaming,
                    showsBranch: false,
                    copyText: text,
                    onRetry: onRetry,
                    onBranch: onBranch)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onHover { isHovering = $0 }
        .task {
            // One stream per row, created on first appearance so a lazily-realised row does
            // not pay for a parse it never shows.
            let stream = markdown ?? MarkdownStream(client: client)
            markdown = stream
            stream.update(text: text, isComplete: !isStreaming)
        }
        .onChange(of: text) { _, newValue in
            markdown?.update(text: newValue, isComplete: !isStreaming)
        }
        .onChange(of: isStreaming) { _, streaming in
            markdown?.update(text: text, isComplete: !streaming)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Assistant")
    }
}
