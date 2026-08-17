import Foundation

/// Synchronous reads (spec 00 §3).
///
/// A query knows its own response type, so `client.query(GetSettings())` is already
/// `Settings` and no call site ever casts. Queries must be cheap; anything expensive is a
/// command.
public protocol CoreQuery: Codable, Sendable, Equatable {
    associatedtype Response: Codable & Sendable
    /// The `type` discriminator on the wire.
    static var queryType: String { get }
}

public struct ListSessions: CoreQuery {
    public typealias Response = SessionList
    public static let queryType = "listSessions"

    public var includeArchived: Bool

    public init(includeArchived: Bool = false) { self.includeArchived = includeArchived }

    private enum CodingKeys: String, CodingKey { case type, includeArchived }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        includeArchived = try c.decodeIfPresent(Bool.self, forKey: .includeArchived) ?? false
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(includeArchived, forKey: .includeArchived)
    }
}

public struct GetSession: CoreQuery {
    public typealias Response = Session
    public static let queryType = "getSession"

    public var sessionId: String

    public init(sessionId: String) { self.sessionId = sessionId }

    private enum CodingKeys: String, CodingKey { case type, sessionId }

    public init(from decoder: Decoder) throws {
        sessionId = try decoder.container(keyedBy: CodingKeys.self)
            .decode(String.self, forKey: .sessionId)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(sessionId, forKey: .sessionId)
    }
}

public struct SearchSessions: CoreQuery {
    public typealias Response = [SearchHit]
    public static let queryType = "searchSessions"

    public var q: String
    public var limit: Int?

    public init(q: String, limit: Int? = nil) {
        self.q = q
        self.limit = limit
    }

    private enum CodingKeys: String, CodingKey { case type, q, limit }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        q = try c.decode(String.self, forKey: .q)
        limit = try c.decodeIfPresent(Int.self, forKey: .limit)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(q, forKey: .q)
        try c.encodeIfPresent(limit, forKey: .limit)
    }
}

public struct SearchInSession: CoreQuery {
    public typealias Response = [SearchHit]
    public static let queryType = "searchInSession"

    public var sessionId: String
    public var q: String

    public init(sessionId: String, q: String) {
        self.sessionId = sessionId
        self.q = q
    }

    private enum CodingKeys: String, CodingKey { case type, sessionId, q }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        q = try c.decode(String.self, forKey: .q)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(sessionId, forKey: .sessionId)
        try c.encode(q, forKey: .q)
    }
}

public struct GetSettings: CoreQuery {
    public typealias Response = Settings
    public static let queryType = "getSettings"

    public init() {}

    private enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws { self.init() }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
    }
}

public struct GetCatalog: CoreQuery {
    public typealias Response = Catalog
    public static let queryType = "getCatalog"

    public init() {}

    private enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws { self.init() }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
    }
}

public struct GetStats: CoreQuery {
    public typealias Response = UsageStats
    public static let queryType = "getStats"

    public var range: StatsRange
    /// IANA timezone id — bucketing by hour is meaningless in UTC (spec 03 §1).
    public var tz: String

    public init(range: StatsRange, tz: String = TimeZone.current.identifier) {
        self.range = range
        self.tz = tz
    }

    private enum CodingKeys: String, CodingKey { case type, range, tz }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        range = try c.decode(StatsRange.self, forKey: .range)
        tz = try c.decode(String.self, forKey: .tz)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(range, forKey: .range)
        try c.encode(tz, forKey: .tz)
    }
}

public struct GetContextUsage: CoreQuery {
    public typealias Response = ContextUsage
    public static let queryType = "getContextUsage"

    public var sessionId: String

    public init(sessionId: String) { self.sessionId = sessionId }

    private enum CodingKeys: String, CodingKey { case type, sessionId }

    public init(from decoder: Decoder) throws {
        sessionId = try decoder.container(keyedBy: CodingKeys.self)
            .decode(String.self, forKey: .sessionId)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(sessionId, forKey: .sessionId)
    }
}

public struct RenderMarkdown: CoreQuery {
    public typealias Response = MarkdownDoc
    public static let queryType = "renderMarkdown"

    public var text: String
    /// `false` while a message is still streaming, so an unterminated fence renders as a
    /// partial code block instead of a broken paragraph (F7.3).
    public var complete: Bool?

    public init(text: String, complete: Bool? = nil) {
        self.text = text
        self.complete = complete
    }

    private enum CodingKeys: String, CodingKey { case type, text, complete }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        text = try c.decode(String.self, forKey: .text)
        complete = try c.decodeIfPresent(Bool.self, forKey: .complete)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(text, forKey: .text)
        try c.encodeIfPresent(complete, forKey: .complete)
    }
}

public struct ResolvePath: CoreQuery {
    public typealias Response = ResolvedPath
    public static let queryType = "resolvePath"

    public var sessionId: String
    public var path: String

    public init(sessionId: String, path: String) {
        self.sessionId = sessionId
        self.path = path
    }

    private enum CodingKeys: String, CodingKey { case type, sessionId, path }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        path = try c.decode(String.self, forKey: .path)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(sessionId, forKey: .sessionId)
        try c.encode(path, forKey: .path)
    }
}

public struct GetAttachment: CoreQuery {
    public typealias Response = Attachment
    public static let queryType = "getAttachment"

    public var attachmentId: String

    public init(attachmentId: String) { self.attachmentId = attachmentId }

    private enum CodingKeys: String, CodingKey { case type, attachmentId }

    public init(from decoder: Decoder) throws {
        attachmentId = try decoder.container(keyedBy: CodingKeys.self)
            .decode(String.self, forKey: .attachmentId)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
        try c.encode(attachmentId, forKey: .attachmentId)
    }
}

public struct ListRecentRoots: CoreQuery {
    public typealias Response = [Workspace]
    public static let queryType = "listRecentRoots"

    public init() {}

    private enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws { self.init() }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.queryType, forKey: .type)
    }
}

// MARK: - Envelope

public struct CoreErrorBody: Codable, Sendable, Equatable, Error, CustomStringConvertible {
    public var code: String
    public var message: String
    public var detail: JSONValue?

    public init(code: String, message: String, detail: JSONValue? = nil) {
        self.code = code
        self.message = message
        self.detail = detail
    }

    public var description: String { "\(code): \(message)" }
}

/// `{"ok": true, "data": …}` or `{"ok": false, "error": {…}}` — the uniform reply to both
/// `query` and `dispatch` (spec 00 §3).
public struct Envelope<T: Decodable & Sendable>: Decodable, Sendable {
    public var ok: Bool
    public var data: T?
    public var error: CoreErrorBody?

    public func value() throws -> T {
        if let error { throw error }
        guard let data else {
            throw CoreErrorBody(code: "empty_response", message: "core returned no data")
        }
        return data
    }
}
