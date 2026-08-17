import AppKit
import FormCore
import Foundation
import UniformTypeIdentifiers

/// The one way an attachment enters a session (F3.1).
///
/// The `+` button, drag-and-drop and paste all land here, so the policy — what a duplicate
/// means, what a rejection looks like, what the tray shows — is written once.
///
/// **Size and type limits are the core's, not this file's.** `addAttachment` returns a typed
/// `attachment_rejected` error for anything over 10 MB or of a disallowed mime, and that
/// message is what the tray renders (F3.6). Duplicating the rule here would guarantee the two
/// drift; the only thing done locally is the *content hash*, because the tray needs it for its
/// own dedupe and for the thumbnail cache key, and it is the same SHA-256 the core stores by.
@MainActor
@Observable
public final class AttachmentIntake {
    /// Newest last. Rejections stay in the list until dismissed, because a rejection the user
    /// never saw is the failure mode F3.6 exists to prevent.
    public private(set) var items: [PendingAttachment] = []

    /// The session attachments are added to. `nil` before a session exists — the composer sets
    /// it as the route changes.
    public var sessionId: String?

    @ObservationIgnored private let stores: CoreStores
    @ObservationIgnored public let thumbnails: ThumbnailStore
    /// Local id → the command that created it, so the `attachment_added` echo can be matched
    /// back to the chip that is waiting for it.
    @ObservationIgnored private var awaiting: [CommandID: UUID] = [:]
    /// Content hash → the thumbnail written for it. Rasterizing and the core's acknowledgement
    /// race; whichever finishes second sends `setAttachmentThumbnail`.
    @ObservationIgnored private var thumbnailPaths: [String: String] = [:]

    public init(stores: CoreStores, thumbnails: ThumbnailStore = .shared) {
        self.stores = stores
        self.thumbnails = thumbnails
        // The core stamps the real data directory on the settings document; pointing the
        // thumbnail cache at `{dataDir}/thumbnails` here means no other workstream has to
        // remember to (spec 13, Part B).
        thumbnails.configure(dataDir: stores.settings.settings.advanced.dataDir)

        // `addAttachment` acks immediately and the record arrives as `attachment_added`, so
        // the chip cannot learn its id without the event stream. `CoreStores` exposes exactly
        // one sink and this is its only consumer today, so the intake claims it on
        // construction rather than making every host remember a wiring line. If a second
        // consumer ever appears, W7's note says the sink becomes a list — take it there
        // rather than adding a second mechanism here.
        stores.onEvent = { [weak self] event in self?.apply(event) }
    }

    // MARK: - Derived

    /// The ids to hand `sendPrompt`, in tray order.
    public var readyAttachmentIds: [String] { items.compactMap(\.attachmentId) }

    public var hasItems: Bool { !items.isEmpty }
    public var isBusy: Bool { items.contains { $0.state == .adding } }

    // MARK: - Intake path 1: the `+` button

    /// Files only, multiple allowed (spec 13, Part B).
    public func presentOpenPanel() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Attach"
        panel.message = "Attach files to this message"
        guard panel.runModal() == .OK else { return }
        add(urls: panel.urls)
    }

    public func add(urls: [URL]) {
        for url in urls { add(.file(url)) }
    }

    // MARK: - Intake path 2: drag and drop

    /// Accepts file URLs and raw image data. Returns whether anything was taken, which is what
    /// `onDrop` needs to report.
    @discardableResult
    public func add(drop providers: [NSItemProvider]) -> Bool {
        var accepted = false
        for provider in providers {
            if provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
                accepted = true
                _ = provider.loadObject(ofClass: URL.self) { [weak self] url, _ in
                    guard let url else { return }
                    Task { @MainActor in self?.add(.file(url)) }
                }
            } else if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                accepted = true
                provider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) {
                    [weak self] data, _ in
                    guard let data else { return }
                    Task { @MainActor in
                        self?.add(
                            .data(data, filename: Self.pastedName(for: "image/png"), mime: "image/png"))
                    }
                }
            }
        }
        return accepted
    }

    // MARK: - Intake path 3: paste

    /// File URLs first, then image data — pasting a file from Finder puts *both* on the
    /// pasteboard, and the file is the better record because it has a name.
    @discardableResult
    public func paste(from pasteboard: NSPasteboard = .general) -> Bool {
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self]) as? [URL],
            !urls.isEmpty {
            add(urls: urls.filter(\.isFileURL))
            return true
        }
        for (type, mime) in Self.pasteboardImageTypes {
            guard let data = pasteboard.data(forType: type) else { continue }
            add(.data(data, filename: Self.pastedName(for: mime), mime: mime))
            return true
        }
        return false
    }

    /// Computed rather than stored: `NSPasteboard.PasteboardType` is not `Sendable`, and a
    /// static `let` of a non-Sendable type is shared mutable state as far as Swift 6 is
    /// concerned.
    private static var pasteboardImageTypes: [(NSPasteboard.PasteboardType, String)] {
        [
            (.png, "image/png"),
            (.tiff, "image/tiff"),
            (NSPasteboard.PasteboardType("public.jpeg"), "image/jpeg"),
        ]
    }

    private static func pastedName(for mime: String) -> String {
        let ext = UTType(mimeType: mime)?.preferredFilenameExtension ?? "png"
        let stamp = ISO8601DateFormatter().string(from: Date())
            .replacingOccurrences(of: ":", with: "-")
        return "Pasted \(stamp).\(ext)"
    }

    // MARK: - The common path

    public func add(_ source: AttachmentSource) {
        Task { await ingest(source) }
    }

    /// The common path, `internal` so tests can await the work `add(_:)` fires and forgets.
    func ingest(_ source: AttachmentSource) async {
        let probe: AttachmentReader.Probe
        do {
            probe = try await Task.detached(priority: .userInitiated) {
                try AttachmentReader.probe(source)
            }.value
        } catch {
            // A file we cannot even read never reaches the core, so this reason is ours.
            append(
                PendingAttachment(
                    id: UUID(), source: source, filename: source.filename, mime: source.mime,
                    bytes: 0, sha256: "",
                    state: .rejected(reason: "Could not read \(source.filename).")))
            return
        }

        // The core dedupes by sha256 (spec 01 §4); showing two chips for one blob would be a
        // lie about what is attached.
        if let existing = items.first(where: { $0.sha256 == probe.sha256 && $0.rejection == nil }) {
            Log.ui.debug("attachment already in tray: \(existing.filename, privacy: .public)")
            return
        }

        let item = PendingAttachment(
            id: UUID(), source: source, filename: source.filename, mime: source.mime,
            bytes: probe.bytes, sha256: probe.sha256, state: .adding)
        append(item)

        // Rasterize and dispatch concurrently — neither needs the other's answer.
        Task { await rasterize(item.id, sha256: probe.sha256, source: source) }

        do {
            let commandId = try await stores.client.dispatch(command(for: source, probe: probe))
            awaiting[commandId] = item.id
        } catch let error as CoreErrorBody {
            // The core's reason, verbatim and inline — deliberately not a toast (F3.6).
            update(item.id) { $0.state = .rejected(reason: Self.reason(from: error)) }
        } catch {
            update(item.id) { $0.state = .rejected(reason: "\(error)") }
        }
    }

    /// Renders the thumbnail, then records its path on the record through
    /// `setAttachmentThumbnail`.
    ///
    /// The path is derivable from the content hash, so a Mac never strictly needs the round
    /// trip — but a Windows or Linux client reading the same store would have no way to know
    /// a thumbnail already exists. Recording it is what makes the cache portable.
    private func rasterize(_ local: UUID, sha256: String, source: AttachmentSource) async {
        guard await thumbnails.thumbnail(sha256: sha256, source: source) != nil else { return }
        thumbnailPaths[sha256] = thumbnails.path(forSHA: sha256)
        guard let attachmentId = items.first(where: { $0.id == local })?.attachmentId else {
            // The record has not come back yet; `apply` records it when it does.
            return
        }
        await recordThumbnail(attachmentId: attachmentId, sha256: sha256)
    }

    private func recordThumbnail(attachmentId: String, sha256: String) async {
        guard let path = thumbnailPaths[sha256] else { return }
        _ = try? await stores.client.dispatch(
            .setAttachmentThumbnail(attachmentId: attachmentId, path: path))
    }

    private func command(for source: AttachmentSource, probe: AttachmentReader.Probe)
        -> CoreCommand {
        switch source {
        case let .file(url):
            return .addAttachment(
                sessionId: sessionId, path: url.path, filename: url.lastPathComponent,
                mime: source.mime)
        case let .data(data, filename, mime):
            return .addAttachment(
                sessionId: sessionId, bytesBase64: data.base64EncodedString(),
                filename: filename, mime: mime)
        }
    }

    /// `attachment rejected: unsupported type: image/x-icon` reads better without the prefix
    /// the `Display` impl adds; the code is what the UI keys on, not the prose.
    private static func reason(from error: CoreErrorBody) -> String {
        let prefix = "attachment rejected: "
        let message = error.message.hasPrefix(prefix)
            ? String(error.message.dropFirst(prefix.count)) : error.message
        return message.prefix(1).uppercased() + message.dropFirst()
    }

    // MARK: - Events
    //
    // `addAttachment` acknowledges immediately and the record arrives as `attachment_added`
    // carrying the same `commandId` (spec 00 §4), so this is where a chip learns its id.

    public func apply(_ event: CoreEvent) {
        switch event.kind {
        case let .attachmentAdded(attachment):
            if let commandId = event.commandId, let local = awaiting.removeValue(forKey: commandId) {
                update(local) { $0.state = .ready(attachmentId: attachment.id) }
            } else if let index = items.firstIndex(where: { $0.sha256 == attachment.sha256 }) {
                // No command id to match on — a record added by another surface for content we
                // are already holding is still the same blob.
                items[index].state = .ready(attachmentId: attachment.id)
            }
            // If the raster finished first it has been waiting for exactly this id.
            if attachment.thumbPath == nil, thumbnailPaths[attachment.sha256] != nil {
                let sha = attachment.sha256
                let id = attachment.id
                Task { await recordThumbnail(attachmentId: id, sha256: sha) }
            }
        case let .attachmentRemoved(attachmentId):
            items.removeAll { $0.attachmentId == attachmentId }
        default:
            break
        }
    }

    // MARK: - Tray edits

    public func remove(_ item: PendingAttachment) {
        items.removeAll { $0.id == item.id }
        awaiting = awaiting.filter { $0.value != item.id }
        guard let attachmentId = item.attachmentId else { return }
        Task { try? await stores.client.dispatch(.removeAttachment(attachmentId: attachmentId)) }
    }

    public func dismissRejections() {
        items.removeAll { $0.rejection != nil }
    }

    /// Called by the composer once `sendPrompt` has gone out: the chips move into the sent
    /// message and the tray goes back to empty. Rejections are dropped with them — the message
    /// they were attached to is gone.
    public func clearAfterSend() {
        items.removeAll()
        awaiting.removeAll()
    }

    // MARK: - Mutation helpers

    private func append(_ item: PendingAttachment) {
        items.append(item)
    }

    private func update(_ id: UUID, _ body: (inout PendingAttachment) -> Void) {
        guard let index = items.firstIndex(where: { $0.id == id }) else { return }
        body(&items[index])
    }

    /// Previews and tests: a tray in a known state with no core behind it.
    public func seed(_ items: [PendingAttachment]) {
        self.items = items
    }
}
