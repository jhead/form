import AppKit
import SwiftUI
import FormCore
import FormDesign

/// A sent prompt: right-aligned, filled, capped at 72 % of the column (F1.2, spec 08 §1).
/// Attachments ride above the text as thumbnail chips (F3.5).
struct UserMessageRow: View {
    @Environment(\.theme) private var theme

    let entry: Entry
    let message: UserMessage
    /// The transcript column's current width; the bubble caps at 72 % of it.
    let columnWidth: CGFloat
    let onRetry: () -> Void
    let onBranch: () -> Void

    @State private var isHovering = false

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
        .accessibilityElement(children: .contain)
        .accessibilityLabel("You said")
    }

    private var bubble: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            let images = message.content.images
            if !images.isEmpty {
                HStack(spacing: theme.metrics.spacing.sm) {
                    ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                        AttachmentThumbnail(image: image)
                    }
                }
            }

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
}

/// An inline image from a sent message. Decoding and thumbnailing are Swift's side of the
/// line (PRD §4.4); the registry and its disk cache are W13's.
private struct AttachmentThumbnail: View {
    @Environment(\.theme) private var theme

    let image: ImageContent

    var body: some View {
        Group {
            if let decoded = Self.decode(image) {
                Image(nsImage: decoded)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                Image(systemName: "photo")
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textTertiary)
            }
        }
        .frame(width: theme.metrics.thumbnail, height: theme.metrics.thumbnail)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
        .accessibilityLabel("Attached image")
    }

    private static func decode(_ image: ImageContent) -> NSImage? {
        guard let data = Data(base64Encoded: image.data) else { return nil }
        return NSImage(data: data)
    }
}
