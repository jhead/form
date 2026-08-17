import AppKit
import FormCore
import FormDesign
import SwiftUI
import UniformTypeIdentifiers

/// A thumbnail, or the type glyph that stands in for one (F3.2).
///
/// The glyph path is `NSWorkspace.icon(for:)`, which is the same icon Finder shows — a `.zip`
/// looks like a `.zip` rather than like a generic document, without this file knowing anything
/// about file types.
struct AttachmentThumbnailView: View {
    @Environment(\.theme) private var theme

    let store: ThumbnailStore
    let sha256: String
    let source: AttachmentSource
    let mime: String
    var side: CGFloat?

    @State private var image: NSImage?

    private var length: CGFloat { side ?? theme.metrics.thumbnail }

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.medium)
                    .aspectRatio(contentMode: .fill)
            } else {
                Image(nsImage: Self.typeGlyph(mime: mime, source: source))
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .padding(theme.metrics.spacing.xxs)
            }
        }
        .frame(width: length, height: length)
        .clipShape(RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
        )
        .task(id: sha256) {
            if let hit = store.cached(sha256: sha256) {
                image = hit
            } else {
                image = await store.thumbnail(sha256: sha256, source: source)
            }
        }
        .accessibilityHidden(true)
    }

    private static func typeGlyph(mime: String, source: AttachmentSource) -> NSImage {
        if case let .file(url) = source, FileManager.default.fileExists(atPath: url.path) {
            return NSWorkspace.shared.icon(forFile: url.path)
        }
        let type = UTType(mimeType: mime) ?? .data
        return NSWorkspace.shared.icon(for: type)
    }
}
