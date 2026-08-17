import Foundation

/// form's own domain types — the Swift half of
/// `core/crates/form-core/src/protocol/domain.rs` (spec 00 §6). Unlike `Wire.swift` these
/// have no `pi` equivalent and are free to evolve.

// MARK: - Models

public struct ModelRef: Codable, Sendable, Hashable {
    public var providerId: String
    public var modelId: String
    public var thinkingLevel: ThinkingLevel

    public init(providerId: String, modelId: String, thinkingLevel: ThinkingLevel = .off) {
        self.providerId = providerId
        self.modelId = modelId
        self.thinkingLevel = thinkingLevel
    }

    /// `anthropic/claude-opus-5` — display and search form, never a wire form.
    public var slug: String { "\(providerId)/\(modelId)" }
}

// MARK: - Sessions

public struct SessionSummary: Codable, Sendable, Equatable, Identifiable, Hashable {
    public var id: String
    public var title: String
    public var titleIsCustom: Bool
    public var groupId: String?
    public var index: Int
    public var workspaceRoot: String?
    public var modelRef: ModelRef
    public var status: SessionStatus
    public var messageCount: Int64
    public var totalTokens: Int64
    public var archived: Bool
    public var pinned: Bool
    public var createdAt: TimestampMs
    public var updatedAt: TimestampMs

    public init(
        id: String, title: String, titleIsCustom: Bool = false, groupId: String? = nil,
        index: Int = 0, workspaceRoot: String? = nil, modelRef: ModelRef,
        status: SessionStatus = .idle, messageCount: Int64 = 0, totalTokens: Int64 = 0,
        archived: Bool = false, pinned: Bool = false,
        createdAt: TimestampMs = Date.nowMs, updatedAt: TimestampMs = Date.nowMs
    ) {
        self.id = id
        self.title = title
        self.titleIsCustom = titleIsCustom
        self.groupId = groupId
        self.index = index
        self.workspaceRoot = workspaceRoot
        self.modelRef = modelRef
        self.status = status
        self.messageCount = messageCount
        self.totalTokens = totalTokens
        self.archived = archived
        self.pinned = pinned
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    /// The basename of the workspace root, for the composer's folder chip (F4.2).
    public var workspaceName: String? {
        workspaceRoot.map { URL(fileURLWithPath: $0).lastPathComponent }
    }
}

/// A session with its full transcript. `summary` is flattened into this object on the wire.
public struct Session: Codable, Sendable, Equatable, Identifiable {
    public var summary: SessionSummary
    public var entries: [Entry]

    public init(summary: SessionSummary, entries: [Entry] = []) {
        self.summary = summary
        self.entries = entries
    }

    public var id: String { summary.id }

    private enum CodingKeys: String, CodingKey { case entries }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        entries = try c.decodeIfPresent([Entry].self, forKey: .entries) ?? []
        summary = try SessionSummary(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        try summary.encode(to: encoder)
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(entries, forKey: .entries)
    }
}

public struct SessionGroup: Codable, Sendable, Equatable, Identifiable, Hashable {
    public var id: String
    public var name: String
    public var index: Int
    public var collapsed: Bool

    public init(id: String, name: String, index: Int, collapsed: Bool = false) {
        self.id = id
        self.name = name
        self.index = index
        self.collapsed = collapsed
    }
}

public struct SessionList: Codable, Sendable, Equatable {
    public var groups: [SessionGroup]
    public var sessions: [SessionSummary]

    public init(groups: [SessionGroup] = [], sessions: [SessionSummary] = []) {
        self.groups = groups
        self.sessions = sessions
    }
}

// MARK: - Search

/// A highlight range within `snippet`, in **UTF-16 code units** so it applies directly to an
/// `AttributedString` (F13.1).
public struct HighlightRange: Codable, Sendable, Equatable, Hashable {
    public var start: Int
    public var len: Int

    public init(start: Int, len: Int) {
        self.start = start
        self.len = len
    }

    /// `nil` when the range does not fall inside `string`, which a corrupt index would do.
    public func range(in string: String) -> Range<String.Index>? {
        let utf16 = string.utf16
        guard
            let lower = utf16.index(utf16.startIndex, offsetBy: start, limitedBy: utf16.endIndex),
            let upper = utf16.index(lower, offsetBy: len, limitedBy: utf16.endIndex),
            let from = String.Index(lower, within: string),
            let to = String.Index(upper, within: string)
        else { return nil }
        return from..<to
    }
}

public struct SearchHit: Codable, Sendable, Equatable, Identifiable {
    public var sessionId: String
    public var entryId: String?
    public var title: String
    public var snippet: String
    public var highlights: [HighlightRange]
    public var score: Double
    public var timestamp: TimestampMs

    public init(
        sessionId: String, entryId: String? = nil, title: String, snippet: String,
        highlights: [HighlightRange] = [], score: Double, timestamp: TimestampMs
    ) {
        self.sessionId = sessionId
        self.entryId = entryId
        self.title = title
        self.snippet = snippet
        self.highlights = highlights
        self.score = score
        self.timestamp = timestamp
    }

    public var id: String { "\(sessionId)#\(entryId ?? "-")" }
}

// MARK: - Context

public struct ContextSegment: Codable, Sendable, Equatable, Identifiable {
    public var kind: SegmentKind
    public var tokens: Int64

    public init(kind: SegmentKind, tokens: Int64) {
        self.kind = kind
        self.tokens = tokens
    }

    public var id: String { kind.rawValue }
}

public struct ContextUsage: Codable, Sendable, Equatable {
    public var sessionId: String
    public var used: Int64
    public var total: Int64
    public var segments: [ContextSegment]
    /// Cumulative cost for the session (F10.3).
    public var cost: Cost
    public var messageCount: Int64

    public init(
        sessionId: String, used: Int64, total: Int64, segments: [ContextSegment] = [],
        cost: Cost = .zero, messageCount: Int64 = 0
    ) {
        self.sessionId = sessionId
        self.used = used
        self.total = total
        self.segments = segments
        self.cost = cost
        self.messageCount = messageCount
    }

    /// Both sides agree on this so the ring and the core never disagree (spec 04 §3).
    public var fraction: Double {
        total == 0 ? 0 : min(1, max(0, Double(used) / Double(total)))
    }
}

// MARK: - Attachments and workspaces

public struct Attachment: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var sessionId: String?
    public var sha256: String
    public var filename: String
    public var mime: String
    public var bytes: Int64
    public var width: Int?
    public var height: Int?
    public var path: String
    public var thumbPath: String?
    public var createdAt: TimestampMs

    public init(
        id: String, sessionId: String? = nil, sha256: String, filename: String, mime: String,
        bytes: Int64, width: Int? = nil, height: Int? = nil, path: String,
        thumbPath: String? = nil, createdAt: TimestampMs = Date.nowMs
    ) {
        self.id = id
        self.sessionId = sessionId
        self.sha256 = sha256
        self.filename = filename
        self.mime = mime
        self.bytes = bytes
        self.width = width
        self.height = height
        self.path = path
        self.thumbPath = thumbPath
        self.createdAt = createdAt
    }

    public var isImage: Bool { mime.hasPrefix("image/") }
}

public struct Workspace: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var lastUsed: TimestampMs

    public init(path: String, lastUsed: TimestampMs) {
        self.path = path
        self.lastUsed = lastUsed
    }

    public var id: String { path }
    public var name: String { URL(fileURLWithPath: path).lastPathComponent }
}

public struct ResolvedPath: Codable, Sendable, Equatable {
    public var resolved: String
    public var insideRoot: Bool

    public init(resolved: String, insideRoot: Bool) {
        self.resolved = resolved
        self.insideRoot = insideRoot
    }
}
