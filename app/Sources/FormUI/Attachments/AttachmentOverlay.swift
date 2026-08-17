import AppKit
import FormCore
import FormDesign
import SwiftUI

/// One item the overlay can show. Sent attachments come from the core; tray items have not
/// been sent yet, so both forms have to reach the same viewer.
public struct AttachmentPreviewItem: Identifiable, Sendable, Equatable {
    public let id: String
    public var filename: String
    public var mime: String
    public var bytes: Int64
    public var sha256: String
    public var source: AttachmentSource

    public init(
        id: String, filename: String, mime: String, bytes: Int64, sha256: String,
        source: AttachmentSource
    ) {
        self.id = id
        self.filename = filename
        self.mime = mime
        self.bytes = bytes
        self.sha256 = sha256
        self.source = source
    }

    public init(_ attachment: Attachment) {
        self.init(
            id: attachment.id, filename: attachment.filename, mime: attachment.mime,
            bytes: attachment.bytes, sha256: attachment.sha256,
            source: .file(URL(fileURLWithPath: attachment.path)))
    }

    public init(_ pending: PendingAttachment) {
        self.init(
            id: pending.attachmentId ?? pending.id.uuidString, filename: pending.filename,
            mime: pending.mime, bytes: pending.bytes, sha256: pending.sha256,
            source: pending.source)
    }

    var fileURL: URL? {
        if case let .file(url) = source { return url }
        return nil
    }
}

/// The full-size viewer (F3.4): dimmed backdrop, image fit to the window, `Esc` or a click
/// outside to dismiss, `←`/`→` between the attachments of the same message, Reveal in Finder.
///
/// Every one of those is reachable from the keyboard alone, which is the acceptance bar in
/// spec 13's "Done when".
public struct AttachmentOverlay: View {
    @Environment(\.theme) private var theme

    private let items: [AttachmentPreviewItem]
    private let store: ThumbnailStore
    private let onDismiss: () -> Void

    @State private var index: Int
    @State private var full: NSImage?
    @FocusState private var isFocused: Bool

    public init(
        items: [AttachmentPreviewItem],
        selected: AttachmentPreviewItem.ID? = nil,
        store: ThumbnailStore,
        onDismiss: @escaping () -> Void
    ) {
        self.items = items
        self.store = store
        self.onDismiss = onDismiss
        _index = State(initialValue: items.firstIndex { $0.id == selected } ?? 0)
    }

    private var current: AttachmentPreviewItem? {
        items.indices.contains(index) ? items[index] : nil
    }

    public var body: some View {
        ZStack {
            SheetScrim(onTap: onDismiss)

            if let current {
                VStack(spacing: theme.metrics.spacing.lg) {
                    content(for: current)
                    caption(for: current)
                }
                .padding(theme.metrics.spacing.xl2)
                // Clicks on the image itself must not fall through to the scrim's dismiss.
                .contentShape(Rectangle())
                .onTapGesture {}
            }
        }
        .focusable()
        .focused($isFocused)
        .onAppear { isFocused = true }
        .onExitCommand(perform: onDismiss)
        .onMoveCommand { direction in
            switch direction {
            case .left, .up: step(-1)
            case .right, .down: step(1)
            @unknown default: break
            }
        }
        .task(id: current?.id) { await loadFull() }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Attachment viewer")
    }

    @ViewBuilder
    private func content(for item: AttachmentPreviewItem) -> some View {
        if let full {
            Image(nsImage: full)
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .clipShape(
                    RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous))
        } else {
            // Non-raster attachments still get a viewer, so `←`/`→` never dead-ends.
            EmptyState(
                systemImage: "doc",
                title: item.filename,
                message: "\(item.mime) · \(AttachmentFormat.size(item.bytes))"
            ) {
                if item.fileURL != nil {
                    FormButton("Reveal in Finder", systemImage: "folder", action: reveal)
                }
            }
            .frame(maxWidth: theme.metrics.contentMaxWidth)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                    .fill(theme.color.surface)
            )
        }
    }

    private func caption(for item: AttachmentPreviewItem) -> some View {
        HStack(spacing: theme.metrics.spacing.lg) {
            IconButton(
                systemImage: "chevron.left", accessibilityLabel: "Previous attachment",
                action: { step(-1) }
            )
            .disabled(items.count < 2)

            VStack(spacing: theme.metrics.spacing.xxs) {
                Text(item.filename)
                    .typeStyle(theme.typography.uiMedium)
                    .foregroundStyle(theme.color.textPrimary)
                Text(
                    items.count > 1
                        ? "\(index + 1) of \(items.count) · \(AttachmentFormat.size(item.bytes))"
                        : AttachmentFormat.size(item.bytes)
                )
                .typeStyle(theme.typography.micro)
                .tabularFigures()
                .foregroundStyle(theme.color.textTertiary)
            }
            .frame(minWidth: theme.metrics.popoverMaxWidth / 2)

            IconButton(
                systemImage: "chevron.right", accessibilityLabel: "Next attachment",
                action: { step(1) }
            )
            .disabled(items.count < 2)

            IconButton(
                systemImage: "folder", accessibilityLabel: "Reveal in Finder", action: reveal
            )
            .disabled(item.fileURL == nil)

            IconButton(systemImage: "xmark", accessibilityLabel: "Close", action: onDismiss)
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.vertical, theme.metrics.spacing.md)
        .background(
            Capsule(style: .continuous).fill(theme.color.surface)
        )
        .overlay(
            Capsule(style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
    }

    /// Wraps, so `←` on the first item lands on the last rather than doing nothing.
    private func step(_ delta: Int) {
        guard items.count > 1 else { return }
        index = (index + delta + items.count) % items.count
        full = nil
    }

    private func reveal() {
        guard let url = current?.fileURL else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }

    /// Full size, not the thumbnail — but still decoded off the main actor, and only for the
    /// item actually on screen.
    private func loadFull() async {
        guard let item = current, item.mime.hasPrefix("image/") || item.mime == "application/pdf"
        else {
            full = nil
            return
        }
        let source = item.source
        let decoded = await Task.detached(priority: .userInitiated) { () -> Data? in
            switch source {
            case let .file(url): return try? Data(contentsOf: url)
            case let .data(data, _, _): return data
            }
        }.value
        guard let decoded else {
            // Fall back to the cached thumbnail rather than showing nothing.
            full = store.cached(sha256: item.sha256)
            return
        }
        full = NSImage(data: decoded) ?? store.cached(sha256: item.sha256)
    }
}

#Preview("Attachment overlay") {
    AttachmentOverlayPreview()
}

private struct AttachmentOverlayPreview: View {
    @State private var store = ThumbnailStore()

    var body: some View {
        ThemePreview(padding: 0) {
            AttachmentOverlay(
                items: PendingAttachment.previewItems.map(AttachmentPreviewItem.init),
                store: store,
                onDismiss: {}
            )
            .frame(height: 420)
        }
    }
}
