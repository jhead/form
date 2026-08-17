import SwiftUI
import FormCore
import FormDesign
import FormMarkdown

/// A reply: no bubble, full column, markdown from the core (F1.2).
///
/// The row owns one `MarkdownStream` and feeds it this message's text. That is the only
/// place a delta turns into work: everything else in the row is a label. The stream is
/// debounced, so a 450-line response reparses tens of times, not hundreds (spec 10 §2), and
/// `MarkdownView` re-renders only the blocks whose ids changed (spec 11 §4).
struct AssistantMessageRow: View, Equatable {
    /// Passed in rather than read from the environment, because this row is `.equatable()`:
    /// the theme has to be part of the value being compared or a re-render on an appearance
    /// switch would be skipped as a no-op. `TranscriptView` also keys the row's *identity* on
    /// the theme — see the comment there for why both are needed.
    let theme: Theme

    let entry: Entry
    let message: AssistantMessage
    let isStreaming: Bool
    let effort: ThinkingLevel?
    let editor: EditorSettings?
    let client: CoreClient
    let onRetry: () -> Void
    let onBranch: () -> Void

    @State private var markdown: MarkdownStream?
    @State private var isHovering = false

    private var thinking: String { message.thinking }
    private var text: String { message.text }

    /// A still-streaming code fence has no stable content to copy yet (spec 11 §4).
    private var style: MarkdownStyle {
        MarkdownStyle(editor: editor, showsCopyButton: !isStreaming)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            if !thinking.isEmpty {
                ThinkingBlock(text: thinking, effort: effort, isStreaming: isStreaming)
            }

            if !text.isEmpty {
                // The caret rides at the end of the column on the tail block's baseline;
                // `MarkdownView` owns the block layout and always claims the full width, so
                // this is as close to the last glyph as the module's API allows. See the
                // W10 report for the hook that would put it inline.
                HStack(alignment: .bottom, spacing: theme.metrics.spacing.xs) {
                    if let markdown {
                        MarkdownView(doc: markdown.doc, style: style)
                    }
                    if isStreaming {
                        TypingCaret()
                            .padding(.bottom, theme.metrics.spacing.xs)
                    }
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

    /// The row carries action closures, which are never equal, so SwiftUI's own value
    /// comparison would re-run every finished message's body on every delta of the one that
    /// is still streaming. Comparing the data instead is what keeps a transcript of long
    /// responses from re-rendering itself token by token (spec 10 §2).
    nonisolated static func == (a: AssistantMessageRow, b: AssistantMessageRow) -> Bool {
        a.entry.id == b.entry.id && a.isStreaming == b.isStreaming && a.effort == b.effort
            && a.editor == b.editor && a.theme == b.theme && a.message == b.message
    }
}
