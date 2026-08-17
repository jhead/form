import Foundation
import FormCore

/// One tool invocation as the transcript shows it.
///
/// The live picture (`ToolRun`, from `tool_execution_*`) and the persisted one (the
/// `toolResult` entry the harness appends) carry the same facts; a reloaded session only has
/// the latter. Both are folded into this so a row does not care which run it is looking at.
public struct ToolCallDisplay: Identifiable, Equatable, Sendable {
    public var call: ToolCall
    public var run: ToolRun?
    public var result: ToolResultMessage?

    public var id: String { call.id }
    public var name: String { call.name.isEmpty ? (run?.name ?? "tool") : call.name }

    /// A call is running until something reports it finished. A call the model emitted but
    /// that has no execution yet also reads as running — that is what the stream looks like
    /// between `toolcall_end` and `tool_execution_start`.
    public var isRunning: Bool { run?.isRunning ?? (result == nil) }
    public var isError: Bool { run?.isError ?? result?.isError ?? false }
    public var durationMs: Int64? { run?.durationMs }
    /// `0…1` where the stub reports it, `nil` for an indeterminate tool (F6.2).
    public var progress: Double? { run?.progress }

    public var linesAdded: Int64? { detail("linesAdded") }
    public var linesRemoved: Int64? { detail("linesRemoved") }

    /// What the model was shown — rendered as markdown in the expanded row (spec 10 §4).
    public var resultText: String? {
        let text = result?.content.compactMap { $0.asText?.text }.joined() ?? ""
        return text.isEmpty ? nil : text
    }

    /// A result that is a path renders as a file chip rather than as prose.
    public var resultPath: String? {
        guard let path = detailString("path") else { return nil }
        return path
    }

    public var arguments: [String: JSONValue] {
        call.arguments.isEmpty ? (run?.args.objectValue ?? [:]) : call.arguments
    }

    /// The one argument worth putting on the collapsed row: the path, command, pattern or
    /// URL — falling back to the first string the call carries.
    public var argumentSummary: String? {
        let args = arguments
        for key in ["path", "command", "pattern", "url", "query", "filename"] {
            if let value = args[key]?.stringValue { return value }
        }
        return args.sorted { $0.key < $1.key }.compactMap { $0.value.stringValue }.first
    }

    /// The full argument object, pretty-printed for the disclosure.
    public var argumentsJSON: String? {
        let args = arguments
        guard !args.isEmpty else { return nil }
        return prettyJSON(.object(args))
    }

    public var resultJSON: String? {
        guard let value = run?.result ?? result?.details, !value.isNull else { return nil }
        return prettyJSON(value)
    }

    private func detail(_ key: String) -> Int64? {
        run?.result?[key]?.intValue ?? result?.details?[key]?.intValue
    }

    private func detailString(_ key: String) -> String? {
        run?.result?[key]?.stringValue ?? result?.details?[key]?.stringValue
    }

    private func prettyJSON(_ value: JSONValue) -> String? {
        guard let data = try? value.encoded(sortedKeys: true),
            let object = try? JSONSerialization.jsonObject(with: data),
            let pretty = try? JSONSerialization.data(
                withJSONObject: object, options: [.prettyPrinted, .sortedKeys])
        else { return nil }
        return String(decoding: pretty, as: UTF8.self)
    }
}

/// The footer under a turn: `3m 31s · 5.9k tokens` (F1.4).
///
/// Duration comes from the `turn_end` record, which only exists for runs this process
/// watched. A reloaded transcript still has the assistant message's own `usage`, so history
/// gets tokens without a duration rather than no footer at all.
public struct TurnFooterModel: Equatable, Sendable {
    public var durationMs: Int64?
    public var totalTokens: Int64
    public var stopReason: StopReason

    public var isEmpty: Bool { durationMs == nil && totalTokens == 0 }

    /// `aborted` and `truncated` are worth saying; a clean stop is not.
    public var note: String? {
        switch stopReason {
        case .aborted: "aborted"
        case .length: "truncated"
        case .error: "failed"
        default: nil
        }
    }
}

/// A provider- or runtime-level failure, rendered inline with a retry (spec 10 §3).
public struct InlineErrorModel: Equatable, Sendable {
    public var code: String?
    public var message: String

    /// The harness formats failures as `overloaded_error: the model is temporarily
    /// overloaded…` (spec 02). Split the identifier off so the row can lead with it.
    public init(raw: String) {
        guard let separator = raw.range(of: ": "),
            raw[raw.startIndex..<separator.lowerBound]
                .allSatisfy({ $0.isLetter || $0.isNumber || $0 == "_" || $0 == "." }),
            separator.lowerBound != raw.startIndex
        else {
            code = nil
            message = raw
            return
        }
        code = String(raw[raw.startIndex..<separator.lowerBound])
        message = String(raw[separator.upperBound...])
    }
}

/// What `TranscriptView` iterates. Ids are stable across a re-derive so `LazyVStack` keeps
/// row identity while the run streams.
public enum TranscriptItem: Identifiable, Equatable, Sendable {
    case user(entry: Entry, message: UserMessage)
    case assistant(entry: Entry, message: AssistantMessage, isStreaming: Bool)
    case tools(id: String, calls: [ToolCallDisplay])
    case footer(id: String, model: TurnFooterModel)
    case error(id: String, model: InlineErrorModel, entryId: String)
    case queued(index: Int, text: String)

    public var id: String {
        switch self {
        case let .user(entry, _): "u:\(entry.id)"
        case let .assistant(entry, _, _): "a:\(entry.id)"
        case let .tools(id, _): "t:\(id)"
        case let .footer(id, _): "f:\(id)"
        case let .error(id, _, _): "e:\(id)"
        case let .queued(index, _): "q:\(index)"
        }
    }
}

/// Turns `ChatStore`'s state into rows.
///
/// **Nothing here re-derives the transcript from events** — it reads what the store already
/// applied incrementally (spec 10 §2). It is a pure function so the grouping rules in F1.3
/// and F1.4 can be tested without SwiftUI.
public enum TranscriptBuilder {
    public static func items(
        entries: [Entry],
        toolRuns: [String: ToolRun],
        turns: [TurnRecord],
        streamingEntryId: String?,
        queued: [String],
        showsFooters: Bool
    ) -> [TranscriptItem] {
        var results: [String: ToolResultMessage] = [:]
        for entry in entries {
            if let result = entry.message?.asToolResult { results[result.toolCallId] = result }
        }

        // `turns` accrues only from live `turn_end`s, so it lines up with the *last* N
        // assistant messages. Counting from the end is what makes a partly-live transcript
        // attribute its footers to the right turns.
        let assistantCount = entries.reduce(into: 0) { count, entry in
            if entry.message?.asAssistant != nil { count += 1 }
        }
        var assistantSeen = 0

        var items: [TranscriptItem] = []
        for entry in entries {
            guard let message = entry.message else { continue }
            switch message {
            case let .user(user):
                items.append(.user(entry: entry, message: user))

            case let .assistant(assistant):
                let isStreaming = entry.id == streamingEntryId
                items.append(
                    .assistant(entry: entry, message: assistant, isStreaming: isStreaming))

                let calls = assistant.toolCalls
                    .filter { !$0.id.isEmpty || !$0.name.isEmpty }
                    .map { call in
                        ToolCallDisplay(
                            call: call, run: toolRuns[call.id], result: results[call.id])
                    }
                if !calls.isEmpty {
                    items.append(.tools(id: entry.id, calls: calls))
                }

                if let raw = assistant.errorMessage, assistant.stopReason == .error {
                    items.append(
                        .error(
                            id: entry.id, model: InlineErrorModel(raw: raw), entryId: entry.id))
                }

                let fromEnd = assistantCount - assistantSeen - 1
                assistantSeen += 1
                let record = turns.indices.contains(turns.count - 1 - fromEnd)
                    ? turns[turns.count - 1 - fromEnd] : nil
                let footer = TurnFooterModel(
                    durationMs: record?.durationMs,
                    totalTokens: record?.usage.totalTokens ?? assistant.usage.totalTokens,
                    stopReason: assistant.stopReason
                )
                // A message still streaming has no footer yet; an empty one has nothing to
                // say. Both would otherwise flash an empty line under the text.
                if showsFooters, !isStreaming, !footer.isEmpty {
                    items.append(.footer(id: entry.id, model: footer))
                }

            case .toolResult, .unknown:
                continue  // folded into the tool group above
            }
        }

        for (index, text) in queued.enumerated() {
            items.append(.queued(index: index, text: text))
        }
        return items
    }
}
