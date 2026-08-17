import SwiftUI
import UniformTypeIdentifiers
import FormCore
import FormDesign

/// A sent prompt: right-aligned, filled, capped at 72 % of the column (F1.2, spec 08 §1).
/// Attachments ride above the text as thumbnail chips (F3.5).
struct UserMessageRow: View, Equatable {
    /// Passed in rather than read from the environment, because this row is `.equatable()`:
    /// the theme has to be part of the value being compared or a re-render on an appearance
    /// switch would be skipped as a no-op. `TranscriptView` also keys the row's *identity* on
    /// the theme — see the comment there for why both are needed.
    let theme: Theme

    let entry: Entry
    let message: UserMessage
    /// The transcript column's current width; the bubble caps at 72 % of it.
    let columnWidth: CGFloat
    let onRetry: () -> Void
    let onBranch: () -> Void

    @State private var isHovering = false
    /// Decoded once per message rather than per render — a sent image is a few hundred KB of
    /// base64 and the row re-renders whenever the transcript does.
    @State private var attachments: [AttachmentPreviewItem] = []

    var body: some View {
        HStack(alignment: .bottom, spacing: theme.metrics.spacing.md) {
            Spacer(minLength: theme.metrics.spacing.xl)

            MessageActions(
                timestamp: message.timestamp,
                isVisible: isHovering,
                copyText: message.content.plainText,
                onRetry: onRetry,
                onBranch: onBranch)

            bubble
        }
        .onHover { isHovering = $0 }
        .task(id: entry.id) { attachments = Self.previewItems(entry: entry, message: message) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("You said")
    }

    /// See `AssistantMessageRow.==`: closures are never equal, so without this every bubble
    /// re-renders on every delta of the message being streamed.
    nonisolated static func == (a: UserMessageRow, b: UserMessageRow) -> Bool {
        a.entry.id == b.entry.id && a.columnWidth == b.columnWidth && a.theme == b.theme
            && a.message == b.message
    }

    private var bubble: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            // W13's viewer, so `←`/`→` walks this message's attachments (F3.4).
            SentAttachmentsView(items: attachments)

            Text(message.content.plainText)
                .typeStyle(theme.typography.body)
                .foregroundStyle(theme.color.textPrimary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, theme.metrics.spacing.lg)
        .padding(.horizontal, 14)  // 12/14 padding, spec 08 §1 — between `lg` and `xl`
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                .fill(theme.color.surfaceRaised)
        )
        // The fill hugs the text; this box only caps the width and pushes it to the column's
        // trailing edge (F1.2).
        .frame(
            maxWidth: max(0, columnWidth) * theme.metrics.messageMaxWidthFraction,
            alignment: .trailing)
    }

    /// A sent image arrives in the transcript as an inline `ImageContent` block — the core
    /// folds the bytes in so the message is self-contained (spec 01 §4) and no attachment id
    /// survives into it. The viewer keys its thumbnail cache on a hash, so the entry id plus
    /// the block's position is the stable key here.
    private static func previewItems(entry: Entry, message: UserMessage) -> [
        AttachmentPreviewItem
    ] {
        message.content.images.enumerated().map { index, image in
            let key = "\(entry.id)#\(index)"
            let data = Data(base64Encoded: image.data) ?? Data()
            let name = "attachment-\(index + 1).\(Self.fileExtension(for: image.mimeType))"
            return AttachmentPreviewItem(
                id: key, filename: name, mime: image.mimeType, bytes: Int64(data.count),
                sha256: key, source: .data(data, filename: name, mime: image.mimeType))
        }
    }

    private static func fileExtension(for mime: String) -> String {
        UTType(mimeType: mime)?.preferredFilenameExtension ?? "bin"
    }
}
