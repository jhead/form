import Foundation

/// The outbound stream (spec 00 §5). Every event carries a `timestamp`, optionally the
/// `commandId` of the command that caused it, and a `kind` flattened into the same object.
///
/// Ordering is the core's contract, not ours: `run_start` first, exactly one terminal
/// `run_end`, `message_update` only between the matching `message_start`/`message_end`.
/// Provider and runtime failures are encoded *in the stream*, never thrown from `dispatch`.
public struct CoreEvent: Codable, Sendable, Equatable {
    public var timestamp: TimestampMs
    public var commandId: CommandID?
    public var kind: CoreEventKind

    public init(timestamp: TimestampMs = Date.nowMs, commandId: CommandID? = nil, kind: CoreEventKind) {
        self.timestamp = timestamp
        self.commandId = commandId
        self.kind = kind
    }

    private enum CodingKeys: String, CodingKey { case timestamp, commandId }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        timestamp = try c.decodeIfPresent(TimestampMs.self, forKey: .timestamp) ?? 0
        commandId = try c.decodeIfPresent(String.self, forKey: .commandId)
        kind = try CoreEventKind(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(timestamp, forKey: .timestamp)
        try c.encodeIfPresent(commandId, forKey: .commandId)
        try kind.encode(to: encoder)
    }

    public var type: String { kind.type }
    public var sessionId: String? { kind.sessionId }
    public var date: Date { Date(msSinceEpoch: timestamp) }
}

public enum CoreEventKind: Sendable, Equatable {
    // Run lifecycle — mirrors `pi`'s `AgentEvent`.
    case runStart(sessionId: String, runId: String)
    case turnStart(sessionId: String, runId: String)
    case messageStart(sessionId: String, entry: Entry)
    case messageUpdate(sessionId: String, entryId: String, event: AssistantMessageEvent)
    case messageEnd(sessionId: String, entry: Entry)
    case toolExecutionStart(
        sessionId: String, toolCallId: String, toolName: String, args: JSONValue)
    case toolExecutionUpdate(sessionId: String, toolCallId: String, partialResult: JSONValue)
    case toolExecutionEnd(
        sessionId: String, toolCallId: String, result: JSONValue, isError: Bool)
    case turnEnd(sessionId: String, runId: String, usage: Usage)
    case runEnd(
        sessionId: String, runId: String, outcome: RunOutcome, usage: Usage, durationMs: Int64)

    // Store and app.
    case sessionCreated(session: SessionSummary)
    case sessionUpdated(session: SessionSummary)
    case sessionDeleted(sessionId: String)
    case groupsChanged(groups: [SessionGroup])
    case settingsChanged(settings: Settings)
    case contextUsageChanged(usage: ContextUsage)
    case statsInvalidated
    case attachmentAdded(attachment: Attachment)
    case attachmentRemoved(attachmentId: String)
    /// Non-fatal; surfaced as a toast (spec 00 §5.2).
    case error(code: String, message: String, detail: JSONValue?)

    /// An event kind this build has never heard of. A core newer than the app degrades to
    /// this rather than crashing it (spec 07 §2).
    case unknown(type: String, raw: JSONValue)

    public var type: String {
        switch self {
        case .runStart: "run_start"
        case .turnStart: "turn_start"
        case .messageStart: "message_start"
        case .messageUpdate: "message_update"
        case .messageEnd: "message_end"
        case .toolExecutionStart: "tool_execution_start"
        case .toolExecutionUpdate: "tool_execution_update"
        case .toolExecutionEnd: "tool_execution_end"
        case .turnEnd: "turn_end"
        case .runEnd: "run_end"
        case .sessionCreated: "session_created"
        case .sessionUpdated: "session_updated"
        case .sessionDeleted: "session_deleted"
        case .groupsChanged: "groups_changed"
        case .settingsChanged: "settings_changed"
        case .contextUsageChanged: "context_usage_changed"
        case .statsInvalidated: "stats_invalidated"
        case .attachmentAdded: "attachment_added"
        case .attachmentRemoved: "attachment_removed"
        case .error: "error"
        case let .unknown(type, _): type
        }
    }

    /// The session an event belongs to, where it has one. `ChatStore` uses it to ignore
    /// traffic for sessions it is not showing.
    public var sessionId: String? {
        switch self {
        case let .runStart(id, _), let .turnStart(id, _), let .messageStart(id, _),
            let .messageUpdate(id, _, _), let .messageEnd(id, _),
            let .toolExecutionStart(id, _, _, _), let .toolExecutionUpdate(id, _, _),
            let .toolExecutionEnd(id, _, _, _), let .turnEnd(id, _, _),
            let .runEnd(id, _, _, _, _), let .sessionDeleted(id):
            id
        case let .sessionCreated(session), let .sessionUpdated(session):
            session.id
        case let .contextUsageChanged(usage):
            usage.sessionId
        default:
            nil
        }
    }
}

extension CoreEventKind: Codable {
    private enum CodingKeys: String, CodingKey {
        case type, sessionId, runId, entry, entryId, event, toolCallId, toolName, args
        case partialResult, result, isError, usage, outcome, durationMs, session, groups
        case settings, attachment, attachmentId, code, message, detail
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)

        func session() throws -> String { try c.decode(String.self, forKey: .sessionId) }
        func run() throws -> String { try c.decode(String.self, forKey: .runId) }
        func toolCall() throws -> String { try c.decode(String.self, forKey: .toolCallId) }

        switch type {
        case "run_start":
            self = .runStart(sessionId: try session(), runId: try run())
        case "turn_start":
            self = .turnStart(sessionId: try session(), runId: try run())
        case "message_start":
            self = .messageStart(
                sessionId: try session(), entry: try c.decode(Entry.self, forKey: .entry))
        case "message_update":
            self = .messageUpdate(
                sessionId: try session(),
                entryId: try c.decode(String.self, forKey: .entryId),
                event: try c.decode(AssistantMessageEvent.self, forKey: .event)
            )
        case "message_end":
            self = .messageEnd(
                sessionId: try session(), entry: try c.decode(Entry.self, forKey: .entry))
        case "tool_execution_start":
            self = .toolExecutionStart(
                sessionId: try session(),
                toolCallId: try toolCall(),
                toolName: try c.decode(String.self, forKey: .toolName),
                args: try c.decodeIfPresent(JSONValue.self, forKey: .args) ?? .null
            )
        case "tool_execution_update":
            self = .toolExecutionUpdate(
                sessionId: try session(),
                toolCallId: try toolCall(),
                partialResult: try c.decodeIfPresent(JSONValue.self, forKey: .partialResult) ?? .null
            )
        case "tool_execution_end":
            self = .toolExecutionEnd(
                sessionId: try session(),
                toolCallId: try toolCall(),
                result: try c.decodeIfPresent(JSONValue.self, forKey: .result) ?? .null,
                isError: try c.decodeIfPresent(Bool.self, forKey: .isError) ?? false
            )
        case "turn_end":
            self = .turnEnd(
                sessionId: try session(), runId: try run(),
                usage: try c.decode(Usage.self, forKey: .usage))
        case "run_end":
            self = .runEnd(
                sessionId: try session(),
                runId: try run(),
                outcome: try c.decode(RunOutcome.self, forKey: .outcome),
                usage: try c.decode(Usage.self, forKey: .usage),
                durationMs: try c.decode(Int64.self, forKey: .durationMs)
            )
        case "session_created":
            self = .sessionCreated(session: try c.decode(SessionSummary.self, forKey: .session))
        case "session_updated":
            self = .sessionUpdated(session: try c.decode(SessionSummary.self, forKey: .session))
        case "session_deleted":
            self = .sessionDeleted(sessionId: try session())
        case "groups_changed":
            self = .groupsChanged(groups: try c.decode([SessionGroup].self, forKey: .groups))
        case "settings_changed":
            self = .settingsChanged(settings: try c.decode(Settings.self, forKey: .settings))
        case "context_usage_changed":
            self = .contextUsageChanged(usage: try c.decode(ContextUsage.self, forKey: .usage))
        case "stats_invalidated":
            self = .statsInvalidated
        case "attachment_added":
            self = .attachmentAdded(
                attachment: try c.decode(Attachment.self, forKey: .attachment))
        case "attachment_removed":
            self = .attachmentRemoved(
                attachmentId: try c.decode(String.self, forKey: .attachmentId))
        case "error":
            self = .error(
                code: try c.decode(String.self, forKey: .code),
                message: try c.decode(String.self, forKey: .message),
                detail: try c.decodeIfPresent(JSONValue.self, forKey: .detail)
            )
        default:
            self = .unknown(type: type, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        if case let .unknown(_, raw) = self {
            try raw.encode(to: encoder)
            return
        }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(type, forKey: .type)
        switch self {
        case let .runStart(sessionId, runId), let .turnStart(sessionId, runId):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(runId, forKey: .runId)
        case let .messageStart(sessionId, entry), let .messageEnd(sessionId, entry):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(entry, forKey: .entry)
        case let .messageUpdate(sessionId, entryId, event):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(entryId, forKey: .entryId)
            try c.encode(event, forKey: .event)
        case let .toolExecutionStart(sessionId, toolCallId, toolName, args):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(toolCallId, forKey: .toolCallId)
            try c.encode(toolName, forKey: .toolName)
            try c.encode(args, forKey: .args)
        case let .toolExecutionUpdate(sessionId, toolCallId, partialResult):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(toolCallId, forKey: .toolCallId)
            try c.encode(partialResult, forKey: .partialResult)
        case let .toolExecutionEnd(sessionId, toolCallId, result, isError):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(toolCallId, forKey: .toolCallId)
            try c.encode(result, forKey: .result)
            try c.encode(isError, forKey: .isError)
        case let .turnEnd(sessionId, runId, usage):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(runId, forKey: .runId)
            try c.encode(usage, forKey: .usage)
        case let .runEnd(sessionId, runId, outcome, usage, durationMs):
            try c.encode(sessionId, forKey: .sessionId)
            try c.encode(runId, forKey: .runId)
            try c.encode(outcome, forKey: .outcome)
            try c.encode(usage, forKey: .usage)
            try c.encode(durationMs, forKey: .durationMs)
        case let .sessionCreated(session), let .sessionUpdated(session):
            try c.encode(session, forKey: .session)
        case let .sessionDeleted(sessionId):
            try c.encode(sessionId, forKey: .sessionId)
        case let .groupsChanged(groups):
            try c.encode(groups, forKey: .groups)
        case let .settingsChanged(settings):
            try c.encode(settings, forKey: .settings)
        case let .contextUsageChanged(usage):
            try c.encode(usage, forKey: .usage)
        case .statsInvalidated:
            break
        case let .attachmentAdded(attachment):
            try c.encode(attachment, forKey: .attachment)
        case let .attachmentRemoved(attachmentId):
            try c.encode(attachmentId, forKey: .attachmentId)
        case let .error(code, message, detail):
            try c.encode(code, forKey: .code)
            try c.encode(message, forKey: .message)
            try c.encodeIfPresent(detail, forKey: .detail)
        case .unknown:
            break
        }
    }
}
