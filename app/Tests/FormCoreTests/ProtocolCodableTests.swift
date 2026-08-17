import Foundation
import Testing

@testable import FormCore

/// Hand-written wire samples in exactly the shape `serde` produces for
/// `core/crates/form-core/src/protocol/`. Until `form-cli protocol-dump` lands (W6) these
/// are the drift check; afterwards they stay as the readable statement of what the
/// encoding rules mean.
@Suite("Protocol encoding")
struct ProtocolCodableTests {

    private func roundTrip(_ json: String, as type: (some Codable).Type) throws -> (
        JSONValue, JSONValue
    ) {
        let data = Data(json.utf8)
        let decoded = try JSONDecoder().decode(type, from: data)
        let reencoded = try JSONEncoder().encode(decoded)
        return (try JSONValue(data: data).normalized, try JSONValue(data: reencoded).normalized)
    }

    // MARK: - Tag spelling

    @Test("assistant message events keep pi's snake_case tags, toolcall_* included")
    func assistantEventTags() throws {
        let partial = """
            {"content":[],"api":"anthropic-messages","provider":"anthropic",\
            "model":"claude-opus-5","usage":{"input":0,"output":0,"cacheRead":0,\
            "cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,\
            "cacheWrite":0,"total":0}},"stopReason":"pending","timestamp":1755}
            """

        let samples: [(String, String)] = [
            ("start", #"{"type":"start","partial":\#(partial)}"#),
            (
                "text_delta",
                #"{"type":"text_delta","contentIndex":0,"delta":"hi","partial":\#(partial)}"#
            ),
            (
                "thinking_delta",
                #"{"type":"thinking_delta","contentIndex":0,"delta":"…","partial":\#(partial)}"#
            ),
            (
                "toolcall_start",
                #"{"type":"toolcall_start","contentIndex":2,"partial":\#(partial)}"#
            ),
            (
                "toolcall_delta",
                #"{"type":"toolcall_delta","contentIndex":2,"delta":"{","partial":\#(partial)}"#
            ),
            (
                "toolcall_end",
                #"""
                {"type":"toolcall_end","contentIndex":2,"toolCall":{"id":"toolu_1",\#
                "name":"read","arguments":{"path":"src/main.rs"}},"partial":\#(partial)}
                """#
            ),
        ]

        for (tag, json) in samples {
            let (original, reencoded) = try roundTrip(json, as: AssistantMessageEvent.self)
            #expect(reencoded == original, "\(tag) did not round-trip")

            let event = try JSONDecoder().decode(
                AssistantMessageEvent.self, from: Data(json.utf8))
            #expect(event.type == tag)
        }
    }

    @Test("commands use camelCase tags")
    func commandTags() throws {
        let command = CoreCommand.sendPrompt(sessionId: "ses_1", text: "hello")
        let json = try JSONValue(data: JSONEncoder().encode(command))
        #expect(json["type"]?.stringValue == "sendPrompt")
        #expect(json["sessionId"]?.stringValue == "ses_1")
        #expect(json["attachmentIds"]?.arrayValue?.isEmpty == true)
    }

    @Test("every command round-trips")
    func commandsRoundTrip() throws {
        let commands: [CoreCommand] = [
            .createSession(),
            .createSession(
                groupId: "grp_1", title: "t", workspaceRoot: "/tmp",
                modelRef: ModelRef(providerId: "anthropic", modelId: "m", thinkingLevel: .high)),
            .sendPrompt(sessionId: "s", text: "hi", attachmentIds: ["a1"]),
            .abortRun(sessionId: "s"),
            .renameSession(sessionId: "s", title: "t"),
            .deleteSession(sessionId: "s"),
            .archiveSession(sessionId: "s", archived: true),
            .pinSession(sessionId: "s", pinned: false),
            .moveSession(sessionId: "s", groupId: nil, index: 3),
            .createGroup(name: "g"),
            .renameGroup(groupId: "g", name: "n"),
            .deleteGroup(groupId: "g"),
            .reorderGroup(groupId: "g", index: 1),
            .setGroupCollapsed(groupId: "g", collapsed: true),
            .setSessionModel(
                sessionId: "s",
                modelRef: ModelRef(providerId: "p", modelId: "m", thinkingLevel: .max)),
            .setWorkspaceRoot(sessionId: "s", path: nil),
            .updateSettings(settings: Settings()),
            .addAttachment(
                sessionId: "s", path: "/tmp/a.png", filename: "a.png", mime: "image/png"),
            .removeAttachment(attachmentId: "att"),
            .setAttachmentThumbnail(attachmentId: "att", path: "/tmp/thumb.png"),
            .branchFromMessage(sessionId: "s", entryId: "e"),
            .retryMessage(sessionId: "s", entryId: "e"),
        ]

        for command in commands {
            let data = try JSONEncoder().encode(command)
            let decoded = try JSONDecoder().decode(CoreCommand.self, from: data)
            #expect(decoded == command, "\(command.type) did not round-trip")
        }
    }

    @Test("every query encodes its tag and round-trips")
    func queriesRoundTrip() throws {
        func check<Q: CoreQuery>(_ query: Q) throws {
            let data = try JSONEncoder().encode(query)
            let json = try JSONValue(data: data)
            #expect(json["type"]?.stringValue == Q.queryType)
            #expect(try JSONDecoder().decode(Q.self, from: data) == query)
        }
        try check(ListSessions(includeArchived: true))
        try check(GetSession(sessionId: "s"))
        try check(SearchSessions(q: "term", limit: 10))
        try check(SearchInSession(sessionId: "s", q: "term"))
        try check(GetSettings())
        try check(GetCatalog())
        try check(GetStats(range: .d30, tz: "Europe/London"))
        try check(GetContextUsage(sessionId: "s"))
        try check(RenderMarkdown(text: "# hi", complete: false))
        try check(ResolvePath(sessionId: "s", path: "src/main.rs"))
        try check(GetAttachment(attachmentId: "att"))
        try check(ListRecentRoots())
    }

    // MARK: - Flattening

    @Test("an entry flattens its kind, and a session flattens its summary")
    func flattening() throws {
        let entryJSON = """
            {"id":"ent_1","sessionId":"ses_1","seq":0,"timestamp":1755,"type":"message",\
            "message":{"role":"user","content":"hello","timestamp":1755}}
            """
        let (original, reencoded) = try roundTrip(entryJSON, as: Entry.self)
        #expect(reencoded == original)

        let entry = try JSONDecoder().decode(Entry.self, from: Data(entryJSON.utf8))
        #expect(entry.message?.asUser?.content.plainText == "hello")

        let sessionJSON = """
            {"id":"ses_1","title":"t","titleIsCustom":false,"index":0,\
            "modelRef":{"providerId":"anthropic","modelId":"claude-opus-5",\
            "thinkingLevel":"high"},"status":"idle","messageCount":0,"totalTokens":0,\
            "archived":false,"pinned":false,"createdAt":1,"updatedAt":2,"entries":[]}
            """
        let (sessionOriginal, sessionReencoded) = try roundTrip(sessionJSON, as: Session.self)
        #expect(sessionReencoded == sessionOriginal)
    }

    @Test("a full run event round-trips")
    func eventRoundTrip() throws {
        let json = """
            {"timestamp":1755000000000,"commandId":"cmd_1","type":"run_end",\
            "sessionId":"ses_1","runId":"run_1","outcome":"completed",\
            "usage":{"input":1200,"output":486,"cacheRead":0,"cacheWrite":0,\
            "totalTokens":1686,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,\
            "total":0}},"durationMs":4200}
            """
        let (original, reencoded) = try roundTrip(json, as: CoreEvent.self)
        #expect(reencoded == original)

        let event = try JSONDecoder().decode(CoreEvent.self, from: Data(json.utf8))
        #expect(event.commandId == "cmd_1")
        #expect(event.sessionId == "ses_1")
        guard case let .runEnd(_, _, outcome, _, durationMs) = event.kind else {
            Issue.record("expected run_end, got \(event.kind)")
            return
        }
        #expect(outcome == .completed)
        #expect(durationMs == 4200)
    }

    // MARK: - Omission rules

    @Test("flags Rust skips when false are omitted, not written as false")
    func skippedFalseFlags() throws {
        let thinking = try JSONValue(
            data: JSONEncoder().encode(ThinkingContent(thinking: "x")))
        #expect(thinking["redacted"] == nil)

        let redacted = try JSONValue(
            data: JSONEncoder().encode(ThinkingContent(thinking: "x", redacted: true)))
        #expect(redacted["redacted"]?.boolValue == true)

        let result = try JSONValue(
            data: JSONEncoder().encode(
                ToolResultMessage(toolCallId: "t", toolName: "read", timestamp: 1)))
        #expect(result["isError"] == nil)
    }

    @Test("absent optionals are omitted, not null")
    func optionalsAreOmitted() throws {
        let summary = SessionSummary(
            id: "s", title: "t",
            modelRef: ModelRef(providerId: "p", modelId: "m", thinkingLevel: .off))
        let json = try JSONValue(data: JSONEncoder().encode(summary))
        #expect(json["groupId"] == nil)
        #expect(json["workspaceRoot"] == nil)
    }

    // MARK: - Forward compatibility

    @Test("an unknown event type decodes instead of throwing, and re-encodes intact")
    func unknownEvent() throws {
        let json = #"{"type":"something_new","timestamp":7,"payload":{"a":1}}"#
        let event = try JSONDecoder().decode(CoreEvent.self, from: Data(json.utf8))
        guard case let .unknown(type, raw) = event.kind else {
            Issue.record("expected .unknown, got \(event.kind)")
            return
        }
        #expect(type == "something_new")
        #expect(raw["payload"]?["a"]?.intValue == 1)
        #expect(event.timestamp == 7)

        let reencoded = try JSONValue(data: JSONEncoder().encode(event)).normalized
        #expect(reencoded == (try JSONValue(jsonString: json).normalized))
    }

    @Test("an unknown content block decodes instead of throwing")
    func unknownBlock() throws {
        let json = """
            {"content":[{"type":"video","url":"x"}],"api":"a","provider":"p","model":"m",\
            "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,\
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},\
            "stopReason":"stop","timestamp":1}
            """
        let message = try JSONDecoder().decode(AssistantMessage.self, from: Data(json.utf8))
        guard case let .unknown(type, _) = message.content.first else {
            Issue.record("expected an unknown block, got \(String(describing: message.content.first))")
            return
        }
        #expect(type == "video")

        let (original, reencoded) = try roundTrip(json, as: AssistantMessage.self)
        #expect(reencoded == original)
    }

    @Test("an unknown assistant event decodes instead of throwing")
    func unknownAssistantEvent() throws {
        let json = #"{"type":"audio_delta","contentIndex":1,"delta":"…"}"#
        let event = try JSONDecoder().decode(AssistantMessageEvent.self, from: Data(json.utf8))
        #expect(event.type == "audio_delta")
        #expect(event.partial == nil)
    }

    @Test("an unknown enum value survives a round trip")
    func unknownEnumValue() throws {
        let level = try JSONDecoder().decode(ThinkingLevel.self, from: Data("\"ludicrous\"".utf8))
        #expect(level.rawValue == "ludicrous")
        #expect(level != .max)
        let reencoded = String(decoding: try JSONEncoder().encode(level), as: UTF8.self)
        #expect(reencoded == "\"ludicrous\"")
    }

    // MARK: - Settings

    @Test("settings keep fields this build does not know about")
    func settingsPreserveUnknownFields() throws {
        // A document from a newer core: a whole section this build has never heard of, and a
        // field inside one it has.
        let json = """
            {"version":1,"general":{"startupView":"home","confirmOnDelete":true,\
            "autoTitleSessions":true,"telemetry":true,"soundOnComplete":"chime"},\
            "appearance":{"themeMode":"dark","textSizeMultiplier":1,"sidebarWidth":300,\
            "sidebarCollapsed":false,"density":"compact","showTurnFooters":true},\
            "defaults":{"modelRef":{"providerId":"anthropic","modelId":"claude-opus-5",\
            "thinkingLevel":"high"},"systemPrompt":"","toolExecution":"sequential",\
            "queueMode":"interrupt"},"providers":{"anthropic":{"enabled":true,\
            "hasKey":true}},"experiments":{"newSidebar":true}}
            """
        let settings = try JSONDecoder().decode(Settings.self, from: Data(json.utf8))
        #expect(settings.appearance.themeMode == .dark)
        #expect(settings.appearance.density == .compact)
        #expect(settings.general.telemetry)
        #expect(settings.defaults.toolExecution == .sequential)
        #expect(settings.defaults.queueMode == .interrupt)
        #expect(settings.unknown["experiments"]?["newSidebar"]?.boolValue == true)
        #expect(settings.general.unknown["soundOnComplete"]?.stringValue == "chime")

        // Sections the document omitted are materialized from the defaults — the core would
        // have filled them in anyway — but nothing it *did* carry may be lost.
        let reencoded = try JSONValue(data: JSONEncoder().encode(settings)).normalized
        #expect(reencoded["experiments"]?["newSidebar"]?.boolValue == true)
        #expect(reencoded["general"]?["soundOnComplete"]?.stringValue == "chime")
        #expect(reencoded["general"]?["telemetry"]?.boolValue == true)
        #expect(reencoded["defaults"]?["queueMode"]?.stringValue == "interrupt")

        for (key, value) in try #require(try JSONValue(jsonString: json).normalized.objectValue) {
            guard case let .object(members) = value else {
                #expect(reencoded[key] == value, "\(key) was dropped")
                continue
            }
            for (inner, innerValue) in members {
                #expect(reencoded[key]?[inner] == innerValue, "\(key).\(inner) was dropped")
            }
        }
    }

    /// Defaults must match the Rust `Default` impls: a control binds to this before the
    /// first `settings_changed` arrives, and a disagreement shows up as a toggle that jumps
    /// on load.
    @Test("settings defaults match the core's")
    func settingsDefaults() {
        let settings = Settings()
        #expect(settings.general.startupView == .home)
        #expect(settings.general.confirmOnDelete)
        #expect(settings.general.autoTitleSessions)
        #expect(settings.general.telemetry == false)

        #expect(settings.appearance.themeMode == .system)
        #expect(settings.appearance.density == .comfortable)
        #expect(settings.appearance.textSizeMultiplier == 1)
        #expect(settings.appearance.sidebarWidth == 300)
        #expect(settings.appearance.showTurnFooters)

        #expect(settings.defaults.toolExecution == .parallel)
        #expect(settings.defaults.queueMode == .queue)
        #expect(settings.defaults.modelRef.modelId == "claude-opus-5")

        #expect(settings.editor.font == "SF Mono")
        #expect(settings.editor.fontSize == 12)
        #expect(settings.editor.tabWidth == 4)
        #expect(settings.editor.wrapCode == false)
        #expect(settings.editor.showLineNumbers)

        #expect(settings.advanced.logLevel == .info)
        #expect(settings.advanced.harnessSpeed == 1)

        #expect(ProviderSettings().enabled, "the core enables a provider by default")
    }

    @Test("the whole settings document round-trips with nothing in the unknown bag")
    func settingsFullDocument() throws {
        // The core's own fixture, so this fails the moment W4 adds a field this build has no
        // property for — rather than quietly parking it in `unknown`.
        let url = ProtocolFixtures.directory
            .appendingPathComponent("events/settings_changed.json")
        guard let data = try? Data(contentsOf: url) else { return }

        let event = try JSONDecoder().decode(CoreEvent.self, from: data)
        guard case let .settingsChanged(settings) = event.kind else {
            Issue.record("expected settings_changed")
            return
        }
        #expect(settings.unknown.isEmpty, "unmodelled top-level keys: \(settings.unknown.keys)")
        #expect(settings.general.unknown.isEmpty)
        #expect(settings.appearance.unknown.isEmpty)
        #expect(settings.defaults.unknown.isEmpty)
        #expect(settings.editor.unknown.isEmpty)
        #expect(settings.advanced.unknown.isEmpty)
        #expect(settings.providers.values.allSatisfy { $0.unknown.isEmpty })

        #expect(settings.general.telemetry)
        #expect(settings.defaults.toolExecution == .parallel)
        #expect(settings.defaults.queueMode == .queue)
        #expect(settings.shortcuts["session.new"] == "cmd+n")
    }

    @Test("a mutated setting still carries the unknown fields")
    func settingsMutationKeepsUnknownFields() throws {
        var settings = Settings()
        settings.unknown["experiments"] = .object(["newSidebar": .bool(true)])
        settings.appearance.themeMode = .light

        let json = try JSONValue(data: JSONEncoder().encode(settings))
        #expect(json["experiments"]?["newSidebar"]?.boolValue == true)
        #expect(json["appearance"]?["themeMode"]?.stringValue == "light")
    }

    // MARK: - JSONValue

    @Test("normalization drops nulls and compares numbers numerically")
    func normalization() throws {
        let a = try JSONValue(jsonString: #"{"a":1,"b":null,"c":[1,null]}"#).normalized
        let b = try JSONValue(jsonString: #"{"a":1.0,"c":[1.0,null]}"#).normalized
        #expect(a == b)
    }
}
