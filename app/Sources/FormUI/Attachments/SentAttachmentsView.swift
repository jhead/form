import FormCore
import FormDesign
import SwiftUI

/// Attachments as they render inside a sent user bubble (F3.5), above the text.
///
/// Clicking one opens the full-size overlay on that item, with `←`/`→` moving between the
/// attachments of the *same message* — which is why the whole set is handed in rather than one
/// chip at a time.
///
/// W10 hosts this in `UserMessageRow`.
public struct SentAttachmentsView: View {
    @Environment(\.theme) private var theme

    private let items: [AttachmentPreviewItem]
    private let store: ThumbnailStore

    @State private var opened: AttachmentPreviewItem.ID?

    public init(attachments: [Attachment], store: ThumbnailStore = .shared) {
        items = attachments.map(AttachmentPreviewItem.init)
        self.store = store
    }

    public init(items: [AttachmentPreviewItem], store: ThumbnailStore = .shared) {
        self.items = items
        self.store = store
    }

    public var body: some View {
        if !items.isEmpty {
            HStack(spacing: theme.metrics.spacing.sm) {
                ForEach(items) { item in
                    Button { opened = item.id } label: {
                        AttachmentThumbnailView(
                            store: store, sha256: item.sha256, source: item.source,
                            mime: item.mime)
                    }
                    .buttonStyle(.plain)
                    .formTooltip(item.filename, detail: AttachmentFormat.size(item.bytes))
                    .accessibilityLabel("Open \(item.filename)")
                }
            }
            .fullScreenOverlay(isPresented: opened != nil) {
                AttachmentOverlay(
                    items: items, selected: opened, store: store, onDismiss: { opened = nil })
            }
        }
    }
}

extension View {
    /// Puts an overlay over the whole window rather than over the receiver, so the attachment
    /// viewer dims the app instead of dimming one message bubble.
    func fullScreenOverlay(
        isPresented: Bool, @ViewBuilder content: @escaping () -> some View
    ) -> some View {
        overlay {
            if isPresented {
                GeometryReader { _ in content() }
                    .ignoresSafeArea()
                    .frame(
                        minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity)
            }
        }
    }
}

#Preview("Sent attachments") {
    SentAttachmentsPreview()
}

private struct SentAttachmentsPreview: View {
    @State private var store = ThumbnailStore()

    var body: some View {
        ThemePreview {
            SentAttachmentsView(
                items: PendingAttachment.previewItems.map(AttachmentPreviewItem.init),
                store: store)
        }
    }
}
