import AppKit
import CryptoKit
import FormCore
import Foundation
import UniformTypeIdentifiers

/// Where an intake item came from. Both forms end up as `addAttachment`; the core copies the
/// bytes into its content store either way (spec 01 §4).
public enum AttachmentSource: Sendable, Equatable {
    case file(URL)
    /// Pasted image data with no file behind it.
    case data(Data, filename: String, mime: String)

    var filename: String {
        switch self {
        case let .file(url): url.lastPathComponent
        case let .data(_, filename, _): filename
        }
    }

    var mime: String {
        switch self {
        case let .file(url):
            UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                ?? "application/octet-stream"
        case let .data(_, _, mime): mime
        }
    }
}

/// One item in the pre-send tray.
///
/// The local half — name, size, content hash, thumbnail — is resolved in Swift the moment the
/// file is picked, so the chip is complete before the core answers. Only `id` waits on the
/// core, because only the core mints it.
public struct PendingAttachment: Identifiable, Sendable, Equatable {
    public enum State: Sendable, Equatable {
        /// Dispatched; waiting for `attachment_added`.
        case adding
        case ready(attachmentId: String)
        /// The core refused it. The reason is the core's, verbatim (F3.6).
        case rejected(reason: String)
    }

    /// Local identity, stable across the state transitions.
    public let id: UUID
    public var source: AttachmentSource
    public var filename: String
    public var mime: String
    public var bytes: Int64
    /// The same hash the core content-addresses by, computed locally so the thumbnail cache
    /// and the tray's own dedupe agree with it without a round trip.
    public var sha256: String
    public var state: State

    public var isImage: Bool { mime.hasPrefix("image/") }
    public var isPDF: Bool { mime == "application/pdf" }

    public var attachmentId: String? {
        if case let .ready(id) = state { return id }
        return nil
    }

    public var rejection: String? {
        if case let .rejected(reason) = state { return reason }
        return nil
    }

    public var sizeText: String { AttachmentFormat.size(bytes) }
}

public enum AttachmentFormat {
    private static let byteFormatter: ByteCountFormatter = {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        formatter.allowsNonnumericFormatting = false
        return formatter
    }()

    public static func size(_ bytes: Int64) -> String {
        byteFormatter.string(fromByteCount: max(0, bytes))
    }

    /// Truncates in the middle, so both the name and the extension survive (spec 13, tray).
    public static func middleTruncated(_ name: String, limit: Int = 22) -> String {
        guard name.count > limit, limit > 4 else { return name }
        let head = limit / 2 - 1
        let tail = limit - head - 1
        return "\(name.prefix(head))…\(name.suffix(tail))"
    }
}

/// The blocking parts of intake: reading a file's size, hashing it, and turning it into the
/// base64 the byte path needs. All of it is `nonisolated` so it runs off the main actor.
enum AttachmentReader {
    struct Probe: Sendable {
        var bytes: Int64
        var sha256: String
        var data: Data?
    }

    /// Hashes without holding the whole file when it came from disk — the > 10 MB case is the
    /// core's to reject, and we should not have allocated 500 MB to find that out.
    static func probe(_ source: AttachmentSource) throws -> Probe {
        switch source {
        case let .data(data, _, _):
            return Probe(
                bytes: Int64(data.count), sha256: hex(SHA256.hash(data: data)), data: data)
        case let .file(url):
            let size =
                (try url.resourceValues(forKeys: [.fileSizeKey]).fileSize).map(Int64.init) ?? 0
            var hasher = SHA256()
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
                hasher.update(data: chunk)
            }
            return Probe(bytes: size, sha256: hex(hasher.finalize()), data: nil)
        }
    }

    private static func hex(_ digest: SHA256Digest) -> String {
        digest.map { String(format: "%02x", $0) }.joined()
    }
}
