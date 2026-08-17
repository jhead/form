import Foundation

/// Transcript wire types — the Swift half of `core/crates/form-core/src/protocol/wire.rs`,
/// which is itself copied verbatim from `pi-core`. **Structurally identical or it is a bug**
/// (PRD §4.3): same field names, same `camelCase` keys, same `snake_case` event tags with
/// `toolcall_*` spelled exactly that way.
///
/// Every type here is `Sendable`, `Equatable`, and `Identifiable` where it has an id.
/// Keys are the property names — no global key strategy is applied anywhere in this module,
/// because commands are `camelCase` while `AssistantMessageEvent` tags are `snake_case`.

/// Unix milliseconds.
public typealias TimestampMs = Int64

// MARK: - Usage and cost

public struct Cost: Codable, Sendable, Equatable {
    public var input: Double
    public var output: Double
    public var cacheRead: Double
    public var cacheWrite: Double
    public var total: Double

    public init(
        input: Double = 0, output: Double = 0, cacheRead: Double = 0,
        cacheWrite: Double = 0, total: Double = 0
    ) {
        self.input = input
        self.output = output
        self.cacheRead = cacheRead
        self.cacheWrite = cacheWrite
        self.total = total
    }

    public static let zero = Cost()
}

public struct Usage: Codable, Sendable, Equatable {
    public var input: Int64
    public var output: Int64
    public var cacheRead: Int64
    public var cacheWrite: Int64
    public var cacheWrite1h: Int64?
    /// Reasoning tokens when the provider reports them. A subset of `output`.
    public var reasoning: Int64?
    public var totalTokens: Int64
    public var cost: Cost

    public init(
        input: Int64 = 0, output: Int64 = 0, cacheRead: Int64 = 0, cacheWrite: Int64 = 0,
        cacheWrite1h: Int64? = nil, reasoning: Int64? = nil, totalTokens: Int64 = 0,
        cost: Cost = .zero
    ) {
        self.input = input
        self.output = output
        self.cacheRead = cacheRead
        self.cacheWrite = cacheWrite
        self.cacheWrite1h = cacheWrite1h
        self.reasoning = reasoning
        self.totalTokens = totalTokens
        self.cost = cost
    }

    public static let zero = Usage()
}

// MARK: - Content blocks

public struct TextContent: Codable, Sendable, Equatable {
    public var text: String
    public var textSignature: String?

    public init(text: String, textSignature: String? = nil) {
        self.text = text
        self.textSignature = textSignature
    }
}

public struct ThinkingContent: Codable, Sendable, Equatable {
    public var thinking: String
    public var thinkingSignature: String?
    public var redacted: Bool

    public init(thinking: String, thinkingSignature: String? = nil, redacted: Bool = false) {
        self.thinking = thinking
        self.thinkingSignature = thinkingSignature
        self.redacted = redacted
    }

    private enum CodingKeys: String, CodingKey { case thinking, thinkingSignature, redacted }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        thinking = try c.decode(String.self, forKey: .thinking)
        thinkingSignature = try c.decodeIfPresent(String.self, forKey: .thinkingSignature)
        redacted = try c.decodeIfPresent(Bool.self, forKey: .redacted) ?? false
    }

    /// Rust skips this when false; encoding it unconditionally would read as drift.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(thinking, forKey: .thinking)
        try c.encodeIfPresent(thinkingSignature, forKey: .thinkingSignature)
        if redacted { try c.encode(true, forKey: .redacted) }
    }
}

public struct ImageContent: Codable, Sendable, Equatable {
    /// Base64-encoded image data.
    public var data: String
    public var mimeType: String

    public init(data: String, mimeType: String) {
        self.data = data
        self.mimeType = mimeType
    }
}

public struct ToolCall: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var arguments: [String: JSONValue]
    public var thoughtSignature: String?
    public var namespace: String?

    public init(
        id: String, name: String, arguments: [String: JSONValue] = [:],
        thoughtSignature: String? = nil, namespace: String? = nil
    ) {
        self.id = id
        self.name = name
        self.arguments = arguments
        self.thoughtSignature = thoughtSignature
        self.namespace = namespace
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, arguments, thoughtSignature, namespace
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        arguments = try c.decodeIfPresent([String: JSONValue].self, forKey: .arguments) ?? [:]
        thoughtSignature = try c.decodeIfPresent(String.self, forKey: .thoughtSignature)
        namespace = try c.decodeIfPresent(String.self, forKey: .namespace)
    }
}

/// Blocks in an assistant message. Tagged on `type` with `camelCase` tags, payload inlined.
public enum AssistantContent: Sendable, Equatable {
    case text(TextContent)
    case thinking(ThinkingContent)
    case toolCall(ToolCall)
    /// A block kind this build does not know. Rendered as nothing, re-encoded intact.
    case unknown(type: String, raw: JSONValue)

    public static func text(_ text: String) -> AssistantContent { .text(TextContent(text: text)) }

    public var asText: TextContent? { if case let .text(t) = self { return t } else { return nil } }
    public var asThinking: ThinkingContent? {
        if case let .thinking(t) = self { return t } else { return nil }
    }
    public var asToolCall: ToolCall? {
        if case let .toolCall(t) = self { return t } else { return nil }
    }
}

extension AssistantContent: Codable {
    private enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "text": self = .text(try TextContent(from: decoder))
        case "thinking": self = .thinking(try ThinkingContent(from: decoder))
        case "toolCall": self = .toolCall(try ToolCall(from: decoder))
        case let other: self = .unknown(type: other, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .text(v):
            try c.encode("text", forKey: .type)
            try v.encode(to: encoder)
        case let .thinking(v):
            try c.encode("thinking", forKey: .type)
            try v.encode(to: encoder)
        case let .toolCall(v):
            try c.encode("toolCall", forKey: .type)
            try v.encode(to: encoder)
        case let .unknown(_, raw):
            try raw.encode(to: encoder)
        }
    }
}

/// Blocks a model can be *given*: user content and tool results.
public enum InputContent: Sendable, Equatable {
    case text(TextContent)
    case image(ImageContent)
    case unknown(type: String, raw: JSONValue)

    public static func text(_ text: String) -> InputContent { .text(TextContent(text: text)) }

    public var asText: TextContent? { if case let .text(t) = self { return t } else { return nil } }
    public var asImage: ImageContent? {
        if case let .image(i) = self { return i } else { return nil }
    }
}

extension InputContent: Codable {
    private enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "text": self = .text(try TextContent(from: decoder))
        case "image": self = .image(try ImageContent(from: decoder))
        case let other: self = .unknown(type: other, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .text(v):
            try c.encode("text", forKey: .type)
            try v.encode(to: encoder)
        case let .image(v):
            try c.encode("image", forKey: .type)
            try v.encode(to: encoder)
        case let .unknown(_, raw):
            try raw.encode(to: encoder)
        }
    }
}

// MARK: - Messages

/// `string | (TextContent | ImageContent)[]` — untagged, as in `pi`'s TypeScript.
public enum UserContent: Sendable, Equatable, Codable {
    case text(String)
    case blocks([InputContent])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let s = try? c.decode(String.self) {
            self = .text(s)
        } else {
            self = .blocks(try c.decode([InputContent].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case let .text(s): try c.encode(s)
        case let .blocks(b): try c.encode(b)
        }
    }

    public var plainText: String {
        switch self {
        case let .text(s): s
        case let .blocks(b): b.compactMap { $0.asText?.text }.joined()
        }
    }

    public var images: [ImageContent] {
        if case let .blocks(b) = self { return b.compactMap(\.asImage) }
        return []
    }
}

public struct UserMessage: Codable, Sendable, Equatable {
    public var content: UserContent
    public var timestamp: TimestampMs

    public init(content: UserContent, timestamp: TimestampMs) {
        self.content = content
        self.timestamp = timestamp
    }

    public init(text: String, timestamp: TimestampMs = Date.nowMs) {
        self.init(content: .text(text), timestamp: timestamp)
    }
}

public struct AssistantMessageDiagnostic: Codable, Sendable, Equatable {
    public var code: String
    public var message: String
    public var detail: JSONValue?
    public var timestamp: TimestampMs?

    public init(
        code: String, message: String, detail: JSONValue? = nil, timestamp: TimestampMs? = nil
    ) {
        self.code = code
        self.message = message
        self.detail = detail
        self.timestamp = timestamp
    }
}

public struct AssistantMessage: Codable, Sendable, Equatable {
    public var content: [AssistantContent]
    /// API identifier, e.g. `anthropic-messages`.
    public var api: String
    public var provider: String
    public var model: String
    public var responseId: String?
    public var diagnostics: [AssistantMessageDiagnostic]?
    public var usage: Usage
    public var stopReason: StopReason
    public var errorMessage: String?
    public var timestamp: TimestampMs

    public init(
        content: [AssistantContent] = [], api: String, provider: String, model: String,
        responseId: String? = nil, diagnostics: [AssistantMessageDiagnostic]? = nil,
        usage: Usage = .zero, stopReason: StopReason = .pending, errorMessage: String? = nil,
        timestamp: TimestampMs = Date.nowMs
    ) {
        self.content = content
        self.api = api
        self.provider = provider
        self.model = model
        self.responseId = responseId
        self.diagnostics = diagnostics
        self.usage = usage
        self.stopReason = stopReason
        self.errorMessage = errorMessage
        self.timestamp = timestamp
    }

    private enum CodingKeys: String, CodingKey {
        case content, api, provider, model, responseId, diagnostics, usage, stopReason
        case errorMessage, timestamp
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        content = try c.decodeIfPresent([AssistantContent].self, forKey: .content) ?? []
        api = try c.decode(String.self, forKey: .api)
        provider = try c.decode(String.self, forKey: .provider)
        model = try c.decode(String.self, forKey: .model)
        responseId = try c.decodeIfPresent(String.self, forKey: .responseId)
        diagnostics = try c.decodeIfPresent([AssistantMessageDiagnostic].self, forKey: .diagnostics)
        usage = try c.decode(Usage.self, forKey: .usage)
        stopReason = try c.decode(StopReason.self, forKey: .stopReason)
        errorMessage = try c.decodeIfPresent(String.self, forKey: .errorMessage)
        timestamp = try c.decode(TimestampMs.self, forKey: .timestamp)
    }

    public var text: String { content.compactMap { $0.asText?.text }.joined() }
    public var thinking: String { content.compactMap { $0.asThinking?.thinking }.joined() }
    public var toolCalls: [ToolCall] { content.compactMap(\.asToolCall) }
}

public struct ToolResultMessage: Codable, Sendable, Equatable {
    public var toolCallId: String
    public var toolName: String
    public var content: [InputContent]
    public var details: JSONValue?
    public var isError: Bool
    public var timestamp: TimestampMs

    public init(
        toolCallId: String, toolName: String, content: [InputContent] = [],
        details: JSONValue? = nil, isError: Bool = false, timestamp: TimestampMs = Date.nowMs
    ) {
        self.toolCallId = toolCallId
        self.toolName = toolName
        self.content = content
        self.details = details
        self.isError = isError
        self.timestamp = timestamp
    }

    private enum CodingKeys: String, CodingKey {
        case toolCallId, toolName, content, details, isError, timestamp
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        toolCallId = try c.decode(String.self, forKey: .toolCallId)
        toolName = try c.decode(String.self, forKey: .toolName)
        content = try c.decodeIfPresent([InputContent].self, forKey: .content) ?? []
        details = try c.decodeIfPresent(JSONValue.self, forKey: .details)
        isError = try c.decodeIfPresent(Bool.self, forKey: .isError) ?? false
        timestamp = try c.decode(TimestampMs.self, forKey: .timestamp)
    }

    /// `isError` is skipped when false on the Rust side.
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(toolCallId, forKey: .toolCallId)
        try c.encode(toolName, forKey: .toolName)
        try c.encode(content, forKey: .content)
        try c.encodeIfPresent(details, forKey: .details)
        if isError { try c.encode(true, forKey: .isError) }
        try c.encode(timestamp, forKey: .timestamp)
    }
}

/// The transcript union, tagged on `role` with the payload inlined.
public enum Message: Sendable, Equatable {
    case user(UserMessage)
    case assistant(AssistantMessage)
    case toolResult(ToolResultMessage)
    case unknown(role: String, raw: JSONValue)

    public var role: String {
        switch self {
        case .user: "user"
        case .assistant: "assistant"
        case .toolResult: "toolResult"
        case let .unknown(role, _): role
        }
    }

    public var asUser: UserMessage? { if case let .user(m) = self { return m } else { return nil } }
    public var asAssistant: AssistantMessage? {
        if case let .assistant(m) = self { return m } else { return nil }
    }
    public var asToolResult: ToolResultMessage? {
        if case let .toolResult(m) = self { return m } else { return nil }
    }

    public var timestamp: TimestampMs {
        switch self {
        case let .user(m): m.timestamp
        case let .assistant(m): m.timestamp
        case let .toolResult(m): m.timestamp
        case .unknown: 0
        }
    }
}

extension Message: Codable {
    private enum CodingKeys: String, CodingKey { case role }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .role) {
        case "user": self = .user(try UserMessage(from: decoder))
        case "assistant": self = .assistant(try AssistantMessage(from: decoder))
        case "toolResult": self = .toolResult(try ToolResultMessage(from: decoder))
        case let other: self = .unknown(role: other, raw: try JSONValue(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .user(m):
            try c.encode("user", forKey: .role)
            try m.encode(to: encoder)
        case let .assistant(m):
            try c.encode("assistant", forKey: .role)
            try m.encode(to: encoder)
        case let .toolResult(m):
            try c.encode("toolResult", forKey: .role)
            try m.encode(to: encoder)
        case let .unknown(_, raw):
            try raw.encode(to: encoder)
        }
    }
}

// MARK: - The streamed assistant protocol

/// Tags are `snake_case`, and the tool-call variants are spelled `toolcall_start`,
/// `toolcall_delta`, `toolcall_end` — inherited from `pi`. **Do not normalize them**
/// (spec 00 §1.2).
///
/// Non-terminal events carry `partial`, the message as the core believes it stands.
/// `ChatStore` renders from the deltas and reconciles against `partial`; it never re-parses
/// the transcript per event.
public enum AssistantMessageEvent: Sendable, Equatable {
    case start(partial: AssistantMessage)
    case textStart(contentIndex: Int, partial: AssistantMessage)
    case textDelta(contentIndex: Int, delta: String, partial: AssistantMessage)
    case textEnd(contentIndex: Int, content: String, partial: AssistantMessage)
    case thinkingStart(contentIndex: Int, partial: AssistantMessage)
    case thinkingDelta(contentIndex: Int, delta: String, partial: AssistantMessage)
    case thinkingEnd(contentIndex: Int, content: String, partial: AssistantMessage)
    case toolCallStart(contentIndex: Int, partial: AssistantMessage)
    case toolCallDelta(contentIndex: Int, delta: String, partial: AssistantMessage)
    case toolCallEnd(contentIndex: Int, toolCall: ToolCall, partial: AssistantMessage)
    case done(reason: DoneReason, message: AssistantMessage)
    case error(reason: ErrorReason, error: AssistantMessage)
    /// An event kind from a newer core. Ignored for rendering, re-encoded intact.
    case unknown(type: String, raw: JSONValue)

    public var partial: AssistantMessage? {
        switch self {
        case let .start(p), let .textStart(_, p), let .textDelta(_, _, p),
            let .textEnd(_, _, p), let .thinkingStart(_, p), let .thinkingDelta(_, _, p),
            let .thinkingEnd(_, _, p), let .toolCallStart(_, p), let .toolCallDelta(_, _, p),
            let .toolCallEnd(_, _, p):
            p
        default: nil
        }
    }

    public var terminalMessage: AssistantMessage? {
        switch self {
        case let .done(_, m), let .error(_, m): m
        default: nil
        }
    }

    public var isTerminal: Bool { terminalMessage != nil }

    public var contentIndex: Int? {
        switch self {
        case let .textStart(i, _), let .textDelta(i, _, _), let .textEnd(i, _, _),
            let .thinkingStart(i, _), let .thinkingDelta(i, _, _), let .thinkingEnd(i, _, _),
            let .toolCallStart(i, _), let .toolCallDelta(i, _, _), let .toolCallEnd(i, _, _):
            i
        default: nil
        }
    }

    public var delta: String? {
        switch self {
        case let .textDelta(_, d, _), let .thinkingDelta(_, d, _), let .toolCallDelta(_, d, _): d
        default: nil
        }
    }

    /// The wire tag. Spelled out rather than derived, because the spelling *is* the contract.
    public var type: String {
        switch self {
        case .start: "start"
        case .textStart: "text_start"
        case .textDelta: "text_delta"
        case .textEnd: "text_end"
        case .thinkingStart: "thinking_start"
        case .thinkingDelta: "thinking_delta"
        case .thinkingEnd: "thinking_end"
        case .toolCallStart: "toolcall_start"
        case .toolCallDelta: "toolcall_delta"
        case .toolCallEnd: "toolcall_end"
        case .done: "done"
        case .error: "error"
        case let .unknown(type, _): type
        }
    }
}

extension AssistantMessageEvent: Codable {
    private enum CodingKeys: String, CodingKey {
        case type, contentIndex, delta, content, toolCall, partial, reason, message, error
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)

        func index() throws -> Int { try c.decode(Int.self, forKey: .contentIndex) }
        func partial() throws -> AssistantMessage {
            try c.decode(AssistantMessage.self, forKey: .partial)
        }
        func delta() throws -> String { try c.decode(String.self, forKey: .delta) }
        func content() throws -> String { try c.decode(String.self, forKey: .content) }

        switch type {
        case "start":
            self = .start(partial: try partial())
        case "text_start":
            self = .textStart(contentIndex: try index(), partial: try partial())
        case "text_delta":
            self = .textDelta(
                contentIndex: try index(), delta: try delta(), partial: try partial())
        case "text_end":
            self = .textEnd(
                contentIndex: try index(), content: try content(), partial: try partial())
        case "thinking_start":
            self = .thinkingStart(contentIndex: try index(), partial: try partial())
        case "thinking_delta":
            self = .thinkingDelta(
                contentIndex: try index(), delta: try delta(), partial: try partial())
        case "thinking_end":
            self = .thinkingEnd(
                contentIndex: try index(), content: try content(), partial: try partial())
        case "toolcall_start":
            self = .toolCallStart(contentIndex: try index(), partial: try partial())
        case "toolcall_delta":
            self = .toolCallDelta(
                contentIndex: try index(), delta: try delta(), partial: try partial())
        case "toolcall_end":
            self = .toolCallEnd(
                contentIndex: try index(),
                toolCall: try c.decode(ToolCall.self, forKey: .toolCall),
                partial: try partial()
            )
        case "done":
            self = .done(
                reason: try c.decode(DoneReason.self, forKey: .reason),
                message: try c.decode(AssistantMessage.self, forKey: .message)
            )
        case "error":
            self = .error(
                reason: try c.decode(ErrorReason.self, forKey: .reason),
                error: try c.decode(AssistantMessage.self, forKey: .error)
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
        if let contentIndex { try c.encode(contentIndex, forKey: .contentIndex) }
        if let delta { try c.encode(delta, forKey: .delta) }
        switch self {
        case let .textEnd(_, content, _), let .thinkingEnd(_, content, _):
            try c.encode(content, forKey: .content)
        case let .toolCallEnd(_, toolCall, _):
            try c.encode(toolCall, forKey: .toolCall)
        case let .done(reason, message):
            try c.encode(reason, forKey: .reason)
            try c.encode(message, forKey: .message)
        case let .error(reason, error):
            try c.encode(reason, forKey: .reason)
            try c.encode(error, forKey: .error)
        default:
            break
        }
        if let partial { try c.encode(partial, forKey: .partial) }
    }
}

// MARK: - Entries

/// The append-only transcript log. Tagged on `type` with `snake_case` tags and `camelCase`
/// fields, inlined into `Entry`.
public enum EntryKind: Sendable, Equatable {
    case message(message: Message)
    case modelChange(provider: String, modelId: String)
    case thinkingLevelChange(thinkingLevel: String)
    case compaction(summary: String, tokensBefore: Int64)
    case branchSummary(fromId: String, summary: String)
    case custom(customType: String, data: JSONValue?)
    case unknown(type: String, raw: JSONValue)

    public var type: String {
        switch self {
        case .message: "message"
        case .modelChange: "model_change"
        case .thinkingLevelChange: "thinking_level_change"
        case .compaction: "compaction"
        case .branchSummary: "branch_summary"
        case .custom: "custom"
        case let .unknown(type, _): type
        }
    }

    public var asMessage: Message? {
        if case let .message(m) = self { return m } else { return nil }
    }
}

extension EntryKind: Codable {
    private enum CodingKeys: String, CodingKey {
        case type, message, provider, modelId, thinkingLevel, summary, tokensBefore
        case fromId, customType, data
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "message":
            self = .message(message: try c.decode(Message.self, forKey: .message))
        case "model_change":
            self = .modelChange(
                provider: try c.decode(String.self, forKey: .provider),
                modelId: try c.decode(String.self, forKey: .modelId)
            )
        case "thinking_level_change":
            self = .thinkingLevelChange(
                thinkingLevel: try c.decode(String.self, forKey: .thinkingLevel))
        case "compaction":
            self = .compaction(
                summary: try c.decode(String.self, forKey: .summary),
                tokensBefore: try c.decode(Int64.self, forKey: .tokensBefore)
            )
        case "branch_summary":
            self = .branchSummary(
                fromId: try c.decode(String.self, forKey: .fromId),
                summary: try c.decode(String.self, forKey: .summary)
            )
        case "custom":
            self = .custom(
                customType: try c.decode(String.self, forKey: .customType),
                data: try c.decodeIfPresent(JSONValue.self, forKey: .data)
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
        case let .message(message):
            try c.encode(message, forKey: .message)
        case let .modelChange(provider, modelId):
            try c.encode(provider, forKey: .provider)
            try c.encode(modelId, forKey: .modelId)
        case let .thinkingLevelChange(level):
            try c.encode(level, forKey: .thinkingLevel)
        case let .compaction(summary, tokensBefore):
            try c.encode(summary, forKey: .summary)
            try c.encode(tokensBefore, forKey: .tokensBefore)
        case let .branchSummary(fromId, summary):
            try c.encode(fromId, forKey: .fromId)
            try c.encode(summary, forKey: .summary)
        case let .custom(customType, data):
            try c.encode(customType, forKey: .customType)
            try c.encodeIfPresent(data, forKey: .data)
        case .unknown:
            break
        }
    }
}

public struct Entry: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var sessionId: String
    public var seq: Int64
    public var parentId: String?
    public var timestamp: TimestampMs
    /// Flattened into this object on the wire.
    public var kind: EntryKind

    public init(
        id: String, sessionId: String, seq: Int64, parentId: String? = nil,
        timestamp: TimestampMs, kind: EntryKind
    ) {
        self.id = id
        self.sessionId = sessionId
        self.seq = seq
        self.parentId = parentId
        self.timestamp = timestamp
        self.kind = kind
    }

    private enum CodingKeys: String, CodingKey {
        case id, sessionId, seq, parentId, timestamp
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        sessionId = try c.decode(String.self, forKey: .sessionId)
        seq = try c.decode(Int64.self, forKey: .seq)
        parentId = try c.decodeIfPresent(String.self, forKey: .parentId)
        timestamp = try c.decode(TimestampMs.self, forKey: .timestamp)
        kind = try EntryKind(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(sessionId, forKey: .sessionId)
        try c.encode(seq, forKey: .seq)
        try c.encodeIfPresent(parentId, forKey: .parentId)
        try c.encode(timestamp, forKey: .timestamp)
        try kind.encode(to: encoder)
    }

    public var message: Message? { kind.asMessage }
}

extension Date {
    /// Unix milliseconds — the protocol's only time representation (spec 00 §1.3).
    public static var nowMs: TimestampMs { Int64(Date().timeIntervalSince1970 * 1000) }

    public init(msSinceEpoch: TimestampMs) {
        self.init(timeIntervalSince1970: Double(msSinceEpoch) / 1000)
    }
}
