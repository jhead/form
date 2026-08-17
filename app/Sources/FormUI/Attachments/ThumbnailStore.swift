import AppKit
import FormCore
import Foundation
import PDFKit
import UniformTypeIdentifiers

/// Thumbnails for attachments (F3.2, F3.3).
///
/// Three rules shape this:
///
/// 1. **Never full-decode a large image.** `CGImageSourceCreateThumbnailAtIndex` with
///    `kCGImageSourceThumbnailMaxPixelSize` decodes straight to the target size, so a 40 MP
///    photo costs about as much as a 1 MP one.
/// 2. **Keyed by content hash, not by attachment id.** The same file attached twice is one
///    blob in the core and one PNG here — and the cache survives the record being removed and
///    re-added, and survives relaunch.
/// 3. **Generation is off the main actor.** Only the finished `NSImage` comes back to it.
@MainActor
public final class ThumbnailStore {
    public static let shared = ThumbnailStore()

    /// 128 pt at 2×, per spec 13.
    public static let pointSize: CGFloat = 128
    static let pixelSize = 256

    private var images: [String: NSImage] = [:]
    private var inFlight: [String: Task<NSImage?, Never>] = [:]
    private var directory: URL = ThumbnailStore.fallbackDirectory

    public init() {}

    /// Points the disk cache at `{dataDir}/thumbnails`. The shell calls this once the core has
    /// reported its data directory; until then thumbnails land in the user's cache directory,
    /// which is the right place for a derived artefact anyway.
    public func configure(dataDir: String) {
        guard !dataDir.isEmpty else { return }
        let next = URL(fileURLWithPath: dataDir).appending(path: "thumbnails")
        guard next != directory else { return }
        directory = next
    }

    /// Synchronous hit, for a view that must not flash a placeholder on re-render.
    public func cached(sha256: String) -> NSImage? { images[sha256] }

    /// Memory → disk → generate. Concurrent callers for the same hash share one task, so a
    /// tray of six chips for the same file rasterizes once.
    public func thumbnail(sha256: String, source: AttachmentSource) async -> NSImage? {
        if let hit = images[sha256] { return hit }
        if let task = inFlight[sha256] { return await task.value }

        let file = directory.appending(path: "\(sha256).png")
        let task = Task<NSImage?, Never> { [directory] in
            let data = await Task.detached(priority: .utility) {
                ThumbnailRenderer.load(cache: file) ?? ThumbnailRenderer.render(source, cache: file, in: directory)
            }.value
            return data.flatMap { NSImage(data: $0) }
        }
        inFlight[sha256] = task
        let image = await task.value
        inFlight[sha256] = nil
        if let image { images[sha256] = image }
        return image
    }

    /// The path the core would record for this content, whether or not it has been generated
    /// yet. `Attachment.thumbPath` wins when the core has one.
    public func path(forSHA sha256: String) -> String {
        directory.appending(path: "\(sha256).png").path
    }

    private static var fallbackDirectory: URL {
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appending(path: "dev.jhead.form/thumbnails")
    }
}

/// The pure, off-actor half. Everything here is file and CoreGraphics work with no UI state.
enum ThumbnailRenderer {
    static func load(cache: URL) -> Data? {
        try? Data(contentsOf: cache)
    }

    /// Returns PNG bytes, writing them to `cache` on the way out. `nil` means "no raster form"
    /// — the view falls back to a type glyph (F3.2).
    static func render(_ source: AttachmentSource, cache: URL, in directory: URL) -> Data? {
        guard let image = cgImage(for: source), let data = png(image) else { return nil }
        try? FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true)
        try? data.write(to: cache, options: .atomic)
        return data
    }

    private static func cgImage(for source: AttachmentSource) -> CGImage? {
        switch source {
        case let .data(data, _, mime):
            if mime == "application/pdf" { return pdfFirstPage(data: data, url: nil) }
            return downsample(CGImageSourceCreateWithData(data as CFData, nil))
        case let .file(url):
            if UTType(filenameExtension: url.pathExtension) == .pdf {
                return pdfFirstPage(data: nil, url: url)
            }
            return downsample(CGImageSourceCreateWithURL(url as CFURL, nil))
        }
    }

    /// The whole point of the exercise: decode *to* the thumbnail size rather than decoding
    /// the full image and scaling it down.
    private static func downsample(_ source: CGImageSource?) -> CGImage? {
        guard let source, CGImageSourceGetCount(source) > 0 else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: ThumbnailStore.pixelSize,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    private static func pdfFirstPage(data: Data?, url: URL?) -> CGImage? {
        let document = data.flatMap(PDFDocument.init(data:)) ?? url.flatMap(PDFDocument.init(url:))
        guard let page = document?.page(at: 0) else { return nil }
        let bounds = page.bounds(for: .mediaBox)
        guard bounds.width > 0, bounds.height > 0 else { return nil }
        // Fit the long edge to the thumbnail box so a portrait page and a landscape one cost
        // the same to draw.
        let scale = CGFloat(ThumbnailStore.pixelSize) / max(bounds.width, bounds.height)
        let size = NSSize(width: bounds.width * scale, height: bounds.height * scale)
        let raster = page.thumbnail(of: size, for: .mediaBox)
        var rect = CGRect(origin: .zero, size: size)
        return raster.cgImage(forProposedRect: &rect, context: nil, hints: nil)
    }

    private static func png(_ image: CGImage) -> Data? {
        let output = NSMutableData()
        guard
            let destination = CGImageDestinationCreateWithData(
                output as CFMutableData, UTType.png.identifier as CFString, 1, nil)
        else { return nil }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
    }
}
