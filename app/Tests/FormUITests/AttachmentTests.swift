import AppKit
import CoreGraphics
import FormCore
import Foundation
import Testing
import UniformTypeIdentifiers

@testable import FormUI

/// Spec 13 Part B: intake, dedupe, thumbnails, and the inline rejection.
@MainActor
struct AttachmentTests {
    // MARK: - Fixtures

    private func scratch() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appending(path: "form-attachment-tests/\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// A real PNG, large enough that "downsampled" is a meaningful claim.
    @discardableResult
    private func writePNG(_ url: URL, side: Int = 1200) throws -> URL {
        let context = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        guard let context else { throw CocoaError(.fileWriteUnknown) }
        context.setFillColor(gray: 0.4, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: side, height: side))
        guard let image = context.makeImage() else { throw CocoaError(.fileWriteUnknown) }

        guard
            let destination = CGImageDestinationCreateWithURL(
                url as CFURL, UTType.png.identifier as CFString, 1, nil)
        else { throw CocoaError(.fileWriteUnknown) }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw CocoaError(.fileWriteUnknown) }
        return url
    }

    private func makeIntake() -> (AttachmentIntake, MockTransport, CoreStores) {
        let transport = MockTransport()
        let stores = CoreStores(client: CoreClient(mock: transport))
        let intake = AttachmentIntake(stores: stores, thumbnails: ThumbnailStore())
        intake.sessionId = "ses_test"
        return (intake, transport, stores)
    }

    // MARK: - Hashing

    @Test("the local hash is the SHA-256 the core content-addresses by")
    func probeHashesLikeTheCore() throws {
        // Known vector: SHA-256 of "abc".
        let directory = try scratch()
        let file = directory.appending(path: "abc.txt")
        try Data("abc".utf8).write(to: file)

        let probe = try AttachmentReader.probe(.file(file))
        #expect(
            probe.sha256
                == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        #expect(probe.bytes == 3)

        let inline = try AttachmentReader.probe(
            .data(Data("abc".utf8), filename: "abc.txt", mime: "text/plain"))
        #expect(inline.sha256 == probe.sha256)
    }

    // MARK: - Intake

    @Test("each intake path dispatches addAttachment")
    func intakeDispatches() async throws {
        let directory = try scratch()
        let file = try writePNG(directory.appending(path: "shot.png"), side: 64)
        let (intake, transport, _) = makeIntake()

        await intake.ingest(.file(file))

        #expect(intake.items.count == 1)
        #expect(intake.items[0].filename == "shot.png")
        #expect(intake.items[0].mime == "image/png")
        #expect(intake.items[0].state == .adding)

        let added = transport.commands.compactMap { command -> String? in
            if case let .addAttachment(_, path, _, _, _) = command { return path }
            return nil
        }
        #expect(added == [file.path])
    }

    @Test("the same file attached twice is one chip")
    func dedupesByContentHash() async throws {
        let directory = try scratch()
        let first = try writePNG(directory.appending(path: "a.png"), side: 64)
        // A different name, byte-identical content — which is what the core dedupes on.
        let second = directory.appending(path: "b.png")
        try FileManager.default.copyItem(at: first, to: second)

        let (intake, transport, _) = makeIntake()
        await intake.ingest(.file(first))
        await intake.ingest(.file(second))

        #expect(intake.items.count == 1)
        let addCount = transport.commands.filter {
            if case .addAttachment = $0 { return true }
            return false
        }.count
        #expect(addCount == 1)
    }

    @Test("attachment_added arrives through the CoreStores sink and promotes the chip")
    func eventPromotesToReady() async throws {
        let directory = try scratch()
        let file = try writePNG(directory.appending(path: "c.png"), side: 64)
        let (intake, _, stores) = makeIntake()
        await intake.ingest(.file(file))

        let sha = intake.items[0].sha256
        // Not `intake.apply` directly: the point is that the intake claimed `onEvent`, which
        // is the only route an `attachment_added` has into the tray.
        let sink = try #require(stores.onEvent)
        sink(
            CoreEvent(
                kind: .attachmentAdded(
                    attachment: Attachment(
                        id: "att_new", sessionId: "ses_test", sha256: sha, filename: "c.png",
                        mime: "image/png", bytes: 100, path: file.path))))

        #expect(intake.items[0].attachmentId == "att_new")
        #expect(intake.readyAttachmentIds == ["att_new"])
    }

    @Test("a rendered thumbnail is recorded on the record through the core")
    func thumbnailPathIsRecorded() async throws {
        let directory = try scratch()
        let file = try writePNG(directory.appending(path: "d.png"), side: 300)
        let (intake, transport, stores) = makeIntake()
        await intake.ingest(.file(file))

        let sha = intake.items[0].sha256
        let sink = try #require(stores.onEvent)
        sink(
            CoreEvent(
                kind: .attachmentAdded(
                    attachment: Attachment(
                        id: "att_thumb", sessionId: "ses_test", sha256: sha, filename: "d.png",
                        mime: "image/png", bytes: 100, path: file.path))))

        // The raster and the ack race; either order ends with one setAttachmentThumbnail.
        try await Task.sleep(for: .milliseconds(300))
        let recorded = transport.commands.compactMap { command -> (String, String)? in
            if case let .setAttachmentThumbnail(id, path) = command { return (id, path) }
            return nil
        }
        #expect(recorded.count == 1)
        #expect(recorded.first?.0 == "att_thumb")
        #expect(recorded.first?.1.hasSuffix("\(sha).png") == true)
    }

    @Test("removing a sent chip dispatches removeAttachment")
    func removeDispatches() async throws {
        let (intake, transport, _) = makeIntake()
        intake.seed([
            PendingAttachment(
                id: UUID(), source: .file(URL(fileURLWithPath: "/tmp/x.png")), filename: "x.png",
                mime: "image/png", bytes: 10, sha256: "abc", state: .ready(attachmentId: "att_x"))
        ])
        intake.remove(intake.items[0])
        #expect(intake.items.isEmpty)

        try await Task.sleep(for: .milliseconds(50))
        #expect(
            transport.commands.contains {
                if case let .removeAttachment(id) = $0 { return id == "att_x" }
                return false
            })
    }

    // MARK: - Rejections (F3.6)

    @Test("a rejection stays in the tray with its reason until dismissed")
    func rejectionIsInlineAndDismissible() {
        let (intake, _, _) = makeIntake()
        intake.seed([
            PendingAttachment(
                id: UUID(), source: .file(URL(fileURLWithPath: "/tmp/big.mov")),
                filename: "big.mov", mime: "video/quicktime", bytes: 12 * 1024 * 1024,
                sha256: "d4",
                state: .rejected(reason: "12 MB exceeds the 10 MB limit"))
        ])
        #expect(intake.items[0].rejection == "12 MB exceeds the 10 MB limit")
        // A rejected item contributes nothing to the send.
        #expect(intake.readyAttachmentIds.isEmpty)

        intake.dismissRejections()
        #expect(intake.items.isEmpty)
    }

    @Test("an unreadable file is rejected before it reaches the core")
    func unreadableFileIsRejectedLocally() async throws {
        let (intake, transport, _) = makeIntake()
        await intake.ingest(
            .file(URL(fileURLWithPath: "/tmp/does-not-exist-\(UUID().uuidString).png")))
        #expect(intake.items.count == 1)
        #expect(intake.items[0].rejection != nil)
        #expect(transport.commands.isEmpty)
    }

    // MARK: - Thumbnails (F3.2, F3.3)

    @Test("an image is downsampled, not full-decoded, and cached on disk by hash")
    func thumbnailDownsamplesAndCaches() async throws {
        let directory = try scratch()
        let file = try writePNG(directory.appending(path: "big.png"), side: 1600)
        let cacheDir = directory.appending(path: "thumbnails")
        let sha = try AttachmentReader.probe(.file(file)).sha256
        let cache = cacheDir.appending(path: "\(sha).png")

        let data = ThumbnailRenderer.render(.file(file), cache: cache, in: cacheDir)
        let rendered = try #require(data.flatMap(NSImage.init(data:)))

        // 128 pt @2× — the long edge must be at the thumbnail size, not the source's.
        let longest = max(rendered.size.width, rendered.size.height)
        #expect(longest <= CGFloat(ThumbnailStore.pixelSize))
        #expect(longest > 0)
        #expect(FileManager.default.fileExists(atPath: cache.path))

        // Second time it comes off disk, byte for byte.
        let reloaded = try #require(ThumbnailRenderer.load(cache: cache))
        #expect(reloaded == data)
    }

    @Test("the store rasterizes once per content hash")
    func thumbnailStoreCachesInMemory() async throws {
        let directory = try scratch()
        let file = try writePNG(directory.appending(path: "shared.png"), side: 400)
        let store = ThumbnailStore()
        store.configure(dataDir: directory.path)
        let sha = try AttachmentReader.probe(.file(file)).sha256

        #expect(store.cached(sha256: sha) == nil)
        let first = await store.thumbnail(sha256: sha, source: .file(file))
        #expect(first != nil)
        // The second call is a memory hit — same instance, no work.
        #expect(store.cached(sha256: sha) === first)
        #expect(store.path(forSHA: sha).hasSuffix("thumbnails/\(sha).png"))
    }

    @Test("a type with no raster form yields no thumbnail, so the view falls back to a glyph")
    func nonRasterYieldsNil() throws {
        let directory = try scratch()
        let file = directory.appending(path: "notes.txt")
        try Data("plain text".utf8).write(to: file)
        let cacheDir = directory.appending(path: "thumbnails")
        let data = ThumbnailRenderer.render(
            .file(file), cache: cacheDir.appending(path: "x.png"), in: cacheDir)
        #expect(data == nil)
    }

    // MARK: - Formatting

    @Test("long filenames truncate in the middle so the extension survives")
    func middleTruncation() {
        let truncated = AttachmentFormat.middleTruncated(
            "screen-capture-2026-08-16-at-11-04-22.png", limit: 20)
        #expect(truncated.count <= 21)
        #expect(truncated.hasSuffix(".png"))
        #expect(truncated.contains("…"))
        #expect(AttachmentFormat.middleTruncated("short.png") == "short.png")
    }

    // MARK: - Overlay navigation

    @Test("the overlay's items carry everything the viewer needs from either source")
    func previewItemsBridgeBothSources() {
        let attachment = Attachment(
            id: "att_1", sha256: "aa", filename: "a.png", mime: "image/png", bytes: 10,
            path: "/tmp/a.png")
        let fromCore = AttachmentPreviewItem(attachment)
        #expect(fromCore.id == "att_1")
        #expect(fromCore.fileURL?.path == "/tmp/a.png")

        let pending = PendingAttachment(
            id: UUID(), source: .data(Data([1, 2, 3]), filename: "p.png", mime: "image/png"),
            filename: "p.png", mime: "image/png", bytes: 3, sha256: "bb", state: .adding)
        let fromTray = AttachmentPreviewItem(pending)
        #expect(fromTray.fileURL == nil)
        #expect(fromTray.sha256 == "bb")
    }
}
