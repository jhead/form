import Foundation

/// Swift mirror of `docs/specs/00-protocol.md`.
///
/// **Owner: W7.** What is here now is the subset the end-to-end proof exercises, written the
/// way the rest must be written: explicit `CodingKeys`, no global key strategy (commands are
/// `camelCase` while `AssistantMessageEvent` tags are `snake_case`), and an `.unknown` case
/// so a core newer than the app degrades instead of crashing.
///
/// TODO(W7): complete every command, query and event, and wire up the protocol-fixture
/// round-trip test from spec 06 §3 — that test is the tripwire for Swift/Rust drift.

// MARK: - Config

public struct CoreConfig: Codable, Sendable {
    public var dataDir: String
    public var seedMockData: Bool
    public var logLevel: String
    public var harnessSpeed: Double

    public init(
        dataDir: String,
        seedMockData: Bool = true,
        logLevel: String = "info",
        harnessSpeed: Double = 1.0
    ) {
        self.dataDir = dataDir
        self.seedMockData = seedMockData
        self.logLevel = logLevel
        self.harnessSpeed = harnessSpeed
    }

    /// `~/Library/Application Support/form`.
    public static func defaultDataDir() -> String {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("form").path
    }
}

// MARK: - Envelope

public struct CoreErrorBody: Codable, Sendable, Error, CustomStringConvertible {
    public var code: String
    public var message: String

    public var description: String { "\(code): \(message)" }
}

/// `{"ok": true, "data": …}` or `{"ok": false, "error": {…}}`.
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

// MARK: - Commands and queries

/// Encoded as `{"type": "<tag>", …}`. Cases carry their own payload encoding so the wire
/// shape stays explicit and reviewable against spec 00 §4.
public enum CoreCommand: Encodable, Sendable {
    case createSession(groupId: String? = nil, title: String? = nil, workspaceRoot: String? = nil)
    case sendPrompt(sessionId: String, text: String, attachmentIds: [String] = [])
    case abortRun(sessionId: String)
    case renameSession(sessionId: String, title: String)
    // TODO(W7): the rest of spec 00 §4.

    private enum CodingKeys: String, CodingKey {
        case type, groupId, title, workspaceRoot, sessionId, text, attachmentIds
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .createSession(groupId, title, workspaceRoot):
            try c.encode("createSession", forKey: .type)
            try c.encodeIfPresent(groupId, forKey: .groupId)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encodeIfPresent(workspaceRoot, forKey: .workspaceRoot)
        case let .sendPrompt(sessionId, text, attachmentIds):
            try c.encode("sendPrompt", forKey: .type)
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(text, forKey: .text)
            try c.encode(attachmentIds, forKey: .attachmentIds)
        case let .abortRun(sessionId):
            try c.encode("abortRun", forKey: .type)
            try c.encode(sessionId, forKey: .sessionId)
        case let .renameSession(sessionId, title):
            try c.encode("renameSession", forKey: .type)
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(title, forKey: .title)
        }
    }
}

public struct CommandAck: Decodable, Sendable {
    public var commandId: String
}

/// A query knows its own response type, so call sites never cast.
public protocol CoreQuery: Encodable, Sendable {
    associatedtype Response: Decodable & Sendable
}

public struct ListSessions: CoreQuery {
    public typealias Response = SessionList
    public let type = "listSessions"
    public var includeArchived: Bool = false
    public init(includeArchived: Bool = false) { self.includeArchived = includeArchived }
}

public struct GetContextUsage: CoreQuery {
    public typealias Response = ContextUsage
    public let type = "getContextUsage"
    public var sessionId: String
    public init(sessionId: String) { self.sessionId = sessionId }
}

// MARK: - Domain

public enum SessionStatus: String, Codable, Sendable {
    case idle, streaming, error
}

public struct ModelRef: Codable, Sendable, Hashable {
    public var providerId: String
    public var modelId: String
    public var thinkingLevel: String
}

public struct SessionSummary: Codable, Sendable, Identifiable, Equatable {
    public var id: String
    public var title: String
    public var titleIsCustom: Bool
    public var groupId: String?
    public var index: Int
    public var workspaceRoot: String?
    public var modelRef: ModelRef
    public var status: SessionStatus
    public var messageCount: Int
    public var totalTokens: Int
    public var archived: Bool
    public var pinned: Bool
    public var createdAt: Int64
    public var updatedAt: Int64
}

public struct SessionGroup: Codable, Sendable, Identifiable, Equatable {
    public var id: String
    public var name: String
    public var index: Int
    public var collapsed: Bool
}

public struct SessionList: Codable, Sendable {
    public var groups: [SessionGroup]
    public var sessions: [SessionSummary]
}

public struct ContextSegment: Codable, Sendable, Equatable {
    public var kind: String
    public var tokens: Int
}

public struct ContextUsage: Codable, Sendable, Equatable {
    public var sessionId: String
    public var used: Int
    public var total: Int
    public var segments: [ContextSegment]
    public var messageCount: Int

    public var fraction: Double {
        total == 0 ? 0 : min(1, Double(used) / Double(total))
    }
}

// MARK: - Events

/// The streamed assistant protocol. Tags are `snake_case`, with `toolcall_*` spelled exactly
/// that way — inherited from `pi`. Do not normalize them.
public enum AssistantMessageEvent: Sendable {
    case start
    case textStart(contentIndex: Int)
    case textDelta(contentIndex: Int, delta: String)
    case textEnd(contentIndex: Int, content: String)
    case thinkingStart(contentIndex: Int)
    case thinkingDelta(contentIndex: Int, delta: String)
    case thinkingEnd(contentIndex: Int, content: String)
    case toolCallStart(contentIndex: Int)
    case toolCallDelta(contentIndex: Int, delta: String)
    case toolCallEnd(contentIndex: Int, toolName: String)
    case done(reason: String)
    case error(reason: String)
    case unknown(type: String)
}

extension AssistantMessageEvent: Decodable {
    private enum CodingKeys: String, CodingKey {
        case type, contentIndex, delta, content, reason, toolCall
    }

    private enum ToolCallKeys: String, CodingKey { case name }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        let index = try c.decodeIfPresent(Int.self, forKey: .contentIndex) ?? 0

        switch type {
        case "start": self = .start
        case "text_start": self = .textStart(contentIndex: index)
        case "text_delta":
            self = .textDelta(contentIndex: index, delta: try c.decode(String.self, forKey: .delta))
        case "text_end":
            self = .textEnd(contentIndex: index, content: try c.decode(String.self, forKey: .content))
        case "thinking_start": self = .thinkingStart(contentIndex: index)
        case "thinking_delta":
            self = .thinkingDelta(contentIndex: index, delta: try c.decode(String.self, forKey: .delta))
        case "thinking_end":
            self = .thinkingEnd(contentIndex: index, content: try c.decode(String.self, forKey: .content))
        case "toolcall_start": self = .toolCallStart(contentIndex: index)
        case "toolcall_delta":
            self = .toolCallDelta(contentIndex: index, delta: try c.decode(String.self, forKey: .delta))
        case "toolcall_end":
            let tool = try c.nestedContainer(keyedBy: ToolCallKeys.self, forKey: .toolCall)
            self = .toolCallEnd(contentIndex: index, toolName: try tool.decode(String.self, forKey: .name))
        case "done": self = .done(reason: try c.decode(String.self, forKey: .reason))
        case "error": self = .error(reason: try c.decode(String.self, forKey: .reason))
        default: self = .unknown(type: type)
        }
    }
}

public struct Usage: Codable, Sendable, Equatable {
    public var input: Int
    public var output: Int
    public var totalTokens: Int
}

public enum CoreEvent: Sendable {
    case runStart(sessionId: String, runId: String)
    case turnStart(sessionId: String, runId: String)
    case messageStart(sessionId: String, entryId: String)
    case messageUpdate(sessionId: String, entryId: String, event: AssistantMessageEvent)
    case messageEnd(sessionId: String, entryId: String)
    case toolExecutionStart(sessionId: String, toolCallId: String, toolName: String)
    case toolExecutionEnd(sessionId: String, toolCallId: String, isError: Bool)
    case turnEnd(sessionId: String, runId: String, usage: Usage)
    case runEnd(sessionId: String, runId: String, outcome: String, usage: Usage, durationMs: Int)
    case sessionCreated(session: SessionSummary)
    case sessionUpdated(session: SessionSummary)
    case statsInvalidated
    /// A core newer than the app must not crash it.
    case unknown(type: String)
}

extension CoreEvent: Decodable {
    private enum CodingKeys: String, CodingKey {
        case type, sessionId, runId, entryId, entry, event, toolCallId, toolName
        case isError, usage, outcome, durationMs, session
    }

    private enum EntryKeys: String, CodingKey { case id }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)

        func session() throws -> String { try c.decode(String.self, forKey: .sessionId) }
        func run() throws -> String { try c.decode(String.self, forKey: .runId) }
        func entryId() throws -> String {
            let entry = try c.nestedContainer(keyedBy: EntryKeys.self, forKey: .entry)
            return try entry.decode(String.self, forKey: .id)
        }

        switch type {
        case "run_start": self = .runStart(sessionId: try session(), runId: try run())
        case "turn_start": self = .turnStart(sessionId: try session(), runId: try run())
        case "message_start": self = .messageStart(sessionId: try session(), entryId: try entryId())
        case "message_update":
            self = .messageUpdate(
                sessionId: try session(),
                entryId: try c.decode(String.self, forKey: .entryId),
                event: try c.decode(AssistantMessageEvent.self, forKey: .event)
            )
        case "message_end": self = .messageEnd(sessionId: try session(), entryId: try entryId())
        case "tool_execution_start":
            self = .toolExecutionStart(
                sessionId: try session(),
                toolCallId: try c.decode(String.self, forKey: .toolCallId),
                toolName: try c.decode(String.self, forKey: .toolName)
            )
        case "tool_execution_end":
            self = .toolExecutionEnd(
                sessionId: try session(),
                toolCallId: try c.decode(String.self, forKey: .toolCallId),
                isError: try c.decodeIfPresent(Bool.self, forKey: .isError) ?? false
            )
        case "turn_end":
            self = .turnEnd(
                sessionId: try session(),
                runId: try run(),
                usage: try c.decode(Usage.self, forKey: .usage)
            )
        case "run_end":
            self = .runEnd(
                sessionId: try session(),
                runId: try run(),
                outcome: try c.decode(String.self, forKey: .outcome),
                usage: try c.decode(Usage.self, forKey: .usage),
                durationMs: try c.decode(Int.self, forKey: .durationMs)
            )
        case "session_created":
            self = .sessionCreated(session: try c.decode(SessionSummary.self, forKey: .session))
        case "session_updated":
            self = .sessionUpdated(session: try c.decode(SessionSummary.self, forKey: .session))
        case "stats_invalidated": self = .statsInvalidated
        default: self = .unknown(type: type)
        }
    }
}
