import Foundation
import Testing

@testable import FormCore

/// The Swift/Rust drift tripwire (spec 00 §8, spec 06 §3, spec 07 §2).
///
/// `form-cli protocol-dump` writes one instance of every command, query and event to
/// `core/tests/fixtures/protocol/`. Every one of them is decoded into its Swift type,
/// re-encoded, and compared as normalized JSON. A field the Swift mirror does not know about
/// disappears on the way out and fails here — which is the whole point.
///
/// **Normalization is `null`-dropping only.** Spec 00 §1.5 treats an absent optional and a
/// `null` one as the same value, and the Rust side is inconsistent about which it writes
/// (`Command`'s optional fields have no `skip_serializing_if`, `SessionSummary`'s do).
/// Everything else — key sets, types, numeric values, array order — is compared exactly.
@Suite("Protocol fixtures")
struct ProtocolFixtureTests {

    @Test(
        "every fixture round-trips through its Swift type",
        .enabled(
            if: ProtocolFixtures.files.isEmpty == false,
            """
            no fixtures in core/tests/fixtures/protocol — `form-cli protocol-dump` is W6's \
            and has not landed yet. This test is skipped, not passing.
            """)
    )
    func fixturesRoundTrip() throws {
        var failures: [String] = []

        for file in ProtocolFixtures.files {
            let data = try Data(contentsOf: file)
            let original = try JSONValue(data: data).normalized

            guard let (codec, encoded) = ProtocolFixtures.roundTrip(data, named: file) else {
                failures.append(
                    """
                    \(file.lastPathComponent): no Swift type decodes this fixture. \
                    Either the mirror is missing a case or the file is named in a way the \
                    registry does not recognise.
                    \(String(decoding: data, as: UTF8.self))
                    """)
                continue
            }

            let reencoded = try JSONValue(data: encoded).normalized
            if reencoded != original {
                failures.append(
                    """
                    \(file.lastPathComponent) drifted (decoded as \(codec)):
                      rust:  \(original.canonicalString)
                      swift: \(reencoded.canonicalString)
                    """)
            }
        }

        #expect(failures.isEmpty, "\(failures.count) fixture(s) failed:\n\(failures.joined(separator: "\n\n"))")
    }

    @Test("the fixture directory is where the spec says it is")
    func fixtureDirectoryLocation() {
        // Not an assertion about content — this only reports where the test is looking, so a
        // skip is diagnosable rather than mysterious.
        Log.core.debug("fixtures: \(ProtocolFixtures.directory.path, privacy: .public)")
        #expect(ProtocolFixtures.directory.path.hasSuffix("core/tests/fixtures/protocol"))
    }
}

/// Fixture discovery and the type registry.
enum ProtocolFixtures {
    /// `<repo>/core/tests/fixtures/protocol`, derived from this file's location so it works
    /// regardless of the working directory `swift test` is run from.
    static let directory: URL = {
        URL(fileURLWithPath: #filePath)  // app/Tests/FormCoreTests/ProtocolFixtureTests.swift
            .deletingLastPathComponent()  // FormCoreTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // app
            .deletingLastPathComponent()  // <repo>
            .appendingPathComponent("core/tests/fixtures/protocol")
    }()

    static let files: [URL] = {
        guard
            let walker = FileManager.default.enumerator(
                at: directory, includingPropertiesForKeys: nil)
        else { return [] }
        return walker
            .compactMap { $0 as? URL }
            .filter { $0.pathExtension == "json" }
            .sorted { $0.path < $1.path }
    }()

    /// A decode/re-encode pair for one type, erased so they can live in one table.
    struct Codec {
        let name: String
        let roundTrip: (Data) throws -> Data
    }

    static func codec<T: Codable>(_ type: T.Type, _ name: String) -> Codec {
        Codec(name: name) { data in
            let value = try JSONDecoder().decode(T.self, from: data)
            return try JSONEncoder().encode(value)
        }
    }

    /// Tries the candidates the file's `type` tag, path and name suggest, then everything
    /// else — W6 chooses the file naming, so the registry has to be forgiving about it.
    static func roundTrip(_ data: Data, named file: URL) -> (String, Data)? {
        for codec in candidates(for: data, file: file) {
            if let encoded = try? codec.roundTrip(data) { return (codec.name, encoded) }
        }
        return nil
    }

    private static func candidates(for data: Data, file: URL) -> [Codec] {
        let json = try? JSONValue(data: data)
        let tag = json?["type"]?.stringValue
        let stem = normalize(file.deletingPathExtension().lastPathComponent)
        let path = file.path.lowercased()

        var ordered: [Codec] = []
        func add(_ codec: Codec?) {
            guard let codec, !ordered.contains(where: { $0.name == codec.name }) else { return }
            ordered.append(codec)
        }

        // A `reason` field only ever appears on an AssistantMessageEvent, and `partial` only
        // ever on a non-terminal one — enough to break the `error`/`start` tag collision with
        // the outer event union.
        let looksLikeAssistantEvent =
            json?["partial"] != nil || json?["reason"] != nil
            || path.contains("assistant") || path.contains("message_event")

        if looksLikeAssistantEvent { add(byName["assistantmessageevent"]) }
        if let tag { add(byTag[tag]) }
        add(byName[stem])
        if path.contains("command") { add(byName["command"]) }
        if path.contains("event") { add(byName["event"]) }
        if path.contains("quer") { add(byTag[tag ?? ""]) }
        for codec in all where !ordered.contains(where: { $0.name == codec.name }) {
            ordered.append(codec)
        }
        return ordered
    }

    private static func normalize(_ name: String) -> String {
        name.lowercased().filter { $0.isLetter || $0.isNumber }
    }

    // MARK: - The registry

    static let commandCodec = codec(CoreCommand.self, "CoreCommand")
    static let eventCodec = codec(CoreEvent.self, "CoreEvent")
    static let assistantEventCodec = codec(
        AssistantMessageEvent.self, "AssistantMessageEvent")

    /// Queries are separate types (each knows its `Response`), so they are registered by tag.
    static let queryCodecs: [String: Codec] = [
        ListSessions.queryType: codec(ListSessions.self, "ListSessions"),
        GetSession.queryType: codec(GetSession.self, "GetSession"),
        SearchSessions.queryType: codec(SearchSessions.self, "SearchSessions"),
        SearchInSession.queryType: codec(SearchInSession.self, "SearchInSession"),
        GetSettings.queryType: codec(GetSettings.self, "GetSettings"),
        GetCatalog.queryType: codec(GetCatalog.self, "GetCatalog"),
        GetStats.queryType: codec(GetStats.self, "GetStats"),
        GetContextUsage.queryType: codec(GetContextUsage.self, "GetContextUsage"),
        RenderMarkdown.queryType: codec(RenderMarkdown.self, "RenderMarkdown"),
        ResolvePath.queryType: codec(ResolvePath.self, "ResolvePath"),
        GetAttachment.queryType: codec(GetAttachment.self, "GetAttachment"),
        ListRecentRoots.queryType: codec(ListRecentRoots.self, "ListRecentRoots"),
    ]

    static let commandTags = [
        "createSession", "sendPrompt", "abortRun", "renameSession", "deleteSession",
        "archiveSession", "pinSession", "moveSession", "createGroup", "renameGroup",
        "deleteGroup", "reorderGroup", "setGroupCollapsed", "setSessionModel",
        "setWorkspaceRoot", "updateSettings", "addAttachment", "removeAttachment",
        "branchFromMessage", "retryMessage",
    ]

    static let eventTags = [
        "run_start", "turn_start", "message_start", "message_update", "message_end",
        "tool_execution_start", "tool_execution_update", "tool_execution_end", "turn_end",
        "run_end", "session_created", "session_updated", "session_deleted", "groups_changed",
        "settings_changed", "context_usage_changed", "stats_invalidated", "attachment_added",
        "attachment_removed", "error",
    ]

    static let assistantEventTags = [
        "start", "text_start", "text_delta", "text_end", "thinking_start", "thinking_delta",
        "thinking_end", "toolcall_start", "toolcall_delta", "toolcall_end", "done",
    ]

    static let byTag: [String: Codec] = {
        var table: [String: Codec] = [:]
        for tag in commandTags { table[tag] = commandCodec }
        for tag in eventTags { table[tag] = eventCodec }
        for tag in assistantEventTags where table[tag] == nil { table[tag] = assistantEventCodec }
        for (tag, codec) in queryCodecs { table[tag] = codec }
        return table
    }()

    /// Domain payloads that carry no `type` of their own, keyed by a normalized file stem.
    static let byName: [String: Codec] = [
        "command": commandCodec,
        "corecommand": commandCodec,
        "event": eventCodec,
        "coreevent": eventCodec,
        "eventkind": eventCodec,
        "assistantmessageevent": assistantEventCodec,
        "commandack": codec(CommandAck.self, "CommandAck"),
        "coreconfig": codec(CoreConfig.self, "CoreConfig"),
        "config": codec(CoreConfig.self, "CoreConfig"),
        "session": codec(Session.self, "Session"),
        "sessionsummary": codec(SessionSummary.self, "SessionSummary"),
        "sessiongroup": codec(SessionGroup.self, "SessionGroup"),
        "sessionlist": codec(SessionList.self, "SessionList"),
        "searchhit": codec(SearchHit.self, "SearchHit"),
        "searchhits": codec([SearchHit].self, "[SearchHit]"),
        "contextusage": codec(ContextUsage.self, "ContextUsage"),
        "contextsegment": codec(ContextSegment.self, "ContextSegment"),
        "attachment": codec(Attachment.self, "Attachment"),
        "workspace": codec(Workspace.self, "Workspace"),
        "workspaces": codec([Workspace].self, "[Workspace]"),
        "resolvedpath": codec(ResolvedPath.self, "ResolvedPath"),
        "modelref": codec(ModelRef.self, "ModelRef"),
        "entry": codec(Entry.self, "Entry"),
        "entries": codec([Entry].self, "[Entry]"),
        "message": codec(Message.self, "Message"),
        "usermessage": codec(UserMessage.self, "UserMessage"),
        "assistantmessage": codec(AssistantMessage.self, "AssistantMessage"),
        "toolresultmessage": codec(ToolResultMessage.self, "ToolResultMessage"),
        "assistantcontent": codec(AssistantContent.self, "AssistantContent"),
        "inputcontent": codec(InputContent.self, "InputContent"),
        "toolcall": codec(ToolCall.self, "ToolCall"),
        "usage": codec(Usage.self, "Usage"),
        "cost": codec(Cost.self, "Cost"),
        "settings": codec(Settings.self, "Settings"),
        "catalog": codec(Catalog.self, "Catalog"),
        "provider": codec(Provider.self, "Provider"),
        "model": codec(Model.self, "Model"),
        "usagestats": codec(UsageStats.self, "UsageStats"),
        "stats": codec(UsageStats.self, "UsageStats"),
        "markdowndoc": codec(MarkdownDoc.self, "MarkdownDoc"),
        "markdownblock": codec(MarkdownBlock.self, "MarkdownBlock"),
        "span": codec(Span.self, "Span"),
    ]

    static let all: [Codec] = {
        var seen = Set<String>()
        var result: [Codec] = []
        for codec in Array(byName.values) + Array(queryCodecs.values) + [commandCodec, eventCodec]
        where seen.insert(codec.name).inserted {
            result.append(codec)
        }
        return result
    }()
}
