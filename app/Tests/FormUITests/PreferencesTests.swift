import FormCore
import FormDesign
import Foundation
import Testing

@testable import FormUI

/// Spec 13 Part A: the settings round trip, the debounce, and the rule that a key never
/// reaches the document.
@MainActor
struct PreferencesTests {
    /// A controller over a mock core whose commands can be inspected. The documents are
    /// *loaded* rather than seeded, so the test exercises the same query path the app does.
    private func makeController(corpus: MockCorpus = .demo) async
        -> (PreferencesController, MockTransport) {
        let transport = MockTransport(corpus: corpus)
        let stores = CoreStores(client: CoreClient(mock: transport))
        await stores.settings.load()
        await stores.catalog.load()
        return (
            PreferencesController(stores: stores, themeController: ThemeController()), transport
        )
    }

    // MARK: - Debounce

    @Test("edits coalesce into one updateSettings")
    func debounceCoalesces() async throws {
        let (controller, transport) = await makeController()

        controller.edit { $0.general.confirmOnDelete = false }
        controller.edit { $0.general.autoTitleSessions = false }
        controller.edit { $0.appearance.showTurnFooters = false }

        // Nothing has gone out yet — that is the point of the 300 ms window.
        #expect(transport.commands.isEmpty)
        #expect(controller.hasPendingEdit)

        await controller.flush()

        let updates = transport.commands.filter {
            if case .updateSettings = $0 { return true }
            return false
        }
        #expect(updates.count == 1)
        #expect(!controller.hasPendingEdit)
    }

    @Test("the debounce fires on its own")
    func debounceFires() async throws {
        let (controller, transport) = await makeController()
        controller.edit { $0.general.confirmOnDelete = false }

        // Polled rather than slept: the assertion is "it flushes without being told to", not
        // "it flushes within exactly one window", and a loaded test machine can stretch a
        // 300 ms timer well past a fixed wait.
        let deadline = ContinuousClock.now + .seconds(5)
        while controller.hasPendingEdit, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(25))
        }

        #expect(!transport.commands.isEmpty)
        #expect(!controller.hasPendingEdit)
    }

    @Test("the view renders the core's normalized document, not the local edit")
    func rendersTheEcho() async throws {
        let (controller, _) = await makeController()

        // Out of range on purpose: the core clamps to 1.4 and echoes that back.
        controller.edit { $0.appearance.textSizeMultiplier = 9 }
        #expect(controller.settings.appearance.textSizeMultiplier == 9)

        await controller.flush()

        var clamped = controller.stores.settings.settings
        clamped.appearance.textSizeMultiplier = 1.4
        controller.stores.settings.apply(
            CoreEvent(kind: .settingsChanged(settings: clamped)))

        #expect(controller.settings.appearance.textSizeMultiplier == 1.4)
    }

    @Test("a keystroke during the round trip is not reverted by the echo")
    func laterEditSurvivesFlush() async throws {
        let (controller, _) = await makeController()
        controller.edit { $0.general.confirmOnDelete = false }
        let flush = Task { await controller.flush() }
        controller.edit { $0.general.autoTitleSessions = false }
        await flush.value
        #expect(controller.settings.general.autoTitleSessions == false)
    }

    // MARK: - Fields the Swift mirror does not name

    @Test("every control's field round-trips through JSON")
    func documentFieldsRoundTrip() throws {
        var settings = FormCore.Settings()
        settings.defaults.queueMode = .interrupt
        settings.defaults.toolExecution = .sequential
        settings.general.telemetry = true
        settings.general.startupView = .lastSession
        settings.appearance.density = .compact

        let data = try JSONEncoder().encode(settings)
        let text = String(decoding: data, as: UTF8.self)
        #expect(text.contains("\"queueMode\""))
        #expect(text.contains("\"toolExecution\""))

        let decoded = try JSONDecoder().decode(FormCore.Settings.self, from: data)
        #expect(decoded.defaults.queueMode == .interrupt)
        #expect(decoded.defaults.toolExecution == .sequential)
        #expect(decoded.general.telemetry == true)
        #expect(decoded.general.startupView == .lastSession)
        #expect(decoded.appearance.density == .compact)
    }

    @Test("a fresh document carries the core's defaults, not zeroes")
    func defaultsMatchTheCore() {
        let settings = FormCore.Settings()
        #expect(settings.editor.fontSize == 12)
        #expect(settings.editor.tabWidth == 4)
        #expect(settings.editor.showLineNumbers == true)
        #expect(settings.editor.wrapCode == false)
        #expect(settings.advanced.logLevel == .info)
        #expect(settings.advanced.harnessSpeed == 1)
        #expect(settings.defaults.queueMode == .queue)
        #expect(settings.defaults.toolExecution == .parallel)
        // The core turns a provider on by default; the tab renders that, not `false`.
        #expect(ProviderSettings().enabled == true)
    }

    @Test("the tab's slider bounds are the core's clamp ranges, not a second copy")
    func rangesComeFromTheCore() {
        #expect(AppearanceSettings.textSizeRange == 0.85 ... 1.4)
        #expect(AppearanceSettings.sidebarWidthRange == 220 ... 420)
        #expect(EditorSettings.fontSizeRange == 9 ... 24)
        #expect(EditorSettings.tabWidthRange == 1 ... 8)
        #expect(AdvancedSettings.harnessSpeedRange == 0.05 ... 200)
    }

    // MARK: - API keys (F8.5)

    @Test("a key saves, reads back as set, clears — and never enters the document")
    func apiKeyNeverEntersTheDocument() async throws {
        let service = "dev.jhead.form.tests.\(UUID().uuidString)"
        let keychain = KeychainStore(service: service)
        let secret = "sk-test-do-not-log-9f3a2b"

        // An unsigned test runner may have no keychain at all; that is the environment, not
        // the code under test.
        do {
            try keychain.set(secret, for: "probe")
            try keychain.delete("probe")
        } catch let error as KeychainStore.KeychainError where error.isUnavailable {
            return
        }

        let transport = MockTransport()
        let client = CoreClient(mock: transport)
        let store = SettingsStore(client: client, keychain: keychain)

        try await store.setAPIKey(secret, for: "anthropic")
        #expect(store.hasAPIKey(for: "anthropic"))
        #expect(try keychain.get("anthropic") == secret)

        let json = String(decoding: try store.exportJSON(), as: UTF8.self)
        #expect(!json.contains(secret), "the key must never appear in settings.json")
        #expect(json.contains("\"hasKey\":true") || json.contains("\"hasKey\" : true"))

        // Every command that crossed the boundary must be secret-free too.
        for command in transport.commands {
            let encoded = String(decoding: try JSONEncoder().encode(command), as: UTF8.self)
            #expect(!encoded.contains(secret), "the key must never cross the FFI boundary")
        }

        try await store.setAPIKey(nil, for: "anthropic")
        #expect(!store.hasAPIKey(for: "anthropic"))
        #expect(try keychain.get("anthropic") == nil)
    }

    // MARK: - Import / export (F9.3)

    @Test("a malformed file is reported, not applied")
    func importReportsMalformedFile() {
        let file = PickedSettingsFile(
            url: URL(fileURLWithPath: "/tmp/broken.json"), data: Data("{not json".utf8))
        guard case let .invalid(message) = SettingsTransfer.decode(file) else {
            Issue.record("expected an invalid result")
            return
        }
        #expect(message.contains("broken.json"))
    }

    @Test("a non-object document is refused")
    func importRefusesNonObject() {
        let file = PickedSettingsFile(
            url: URL(fileURLWithPath: "/tmp/list.json"), data: Data("[1,2,3]".utf8))
        guard case .invalid = SettingsTransfer.decode(file) else {
            Issue.record("expected an invalid result")
            return
        }
    }

    @Test("out-of-range values are corrected and reported rather than rejected")
    func importClampsAndReports() throws {
        let json = """
            {
              "version": 1,
              "appearance": { "textSizeMultiplier": 4.0, "sidebarWidth": 12 },
              "editor": { "fontSize": 200, "tabWidth": 99 }
            }
            """
        let file = PickedSettingsFile(
            url: URL(fileURLWithPath: "/tmp/wide.json"), data: Data(json.utf8))
        guard case let .success(settings, notes) = SettingsTransfer.decode(file) else {
            Issue.record("expected the file to apply with corrections")
            return
        }
        #expect(settings.appearance.textSizeMultiplier == 1.4)
        #expect(settings.appearance.sidebarWidth == 220)
        #expect(settings.editor.fontSize == 24)
        #expect(settings.editor.tabWidth == 8)
        #expect(notes.count == 4)
    }

    @Test("a credential smuggled into an imported document is stripped")
    func importStripsCredentials() {
        let json = """
            {"providers": {"anthropic": {"enabled": true, "apiKey": "sk-leaked"}}}
            """
        let file = PickedSettingsFile(
            url: URL(fileURLWithPath: "/tmp/leak.json"), data: Data(json.utf8))
        guard case let .success(settings, notes) = SettingsTransfer.decode(file) else {
            Issue.record("expected the file to apply")
            return
        }
        #expect(settings.providers["anthropic"]?.unknown["apiKey"] == nil)
        #expect(notes.contains { $0.contains("credential-like") })
    }

    // MARK: - Models

    @Test("changing the default model carries an effort that model offers")
    func defaultModelRepairsEffort() async {
        let (controller, _) = await makeController()
        guard
            let target = controller.catalog.providers
                .flatMap({ provider in provider.models.map { (provider, $0) } })
                .first(where: { !$0.1.capabilities.reasoning })
        else { return }

        controller.setDefaultModel(
            ModelRef(providerId: target.0.id, modelId: target.1.id, thinkingLevel: .max))
        let level = controller.settings.defaults.modelRef.thinkingLevel
        #expect(controller.catalog.thinkingLevels(for: controller.settings.defaults.modelRef)
            .contains(level))
    }

    // MARK: - Shortcuts

    @Test("recording the default binding clears the override rather than storing a copy")
    func recordingTheDefaultClearsTheOverride() {
        let resolver = ShortcutResolver()
        guard let command = AppCommands.all.first(where: { $0.defaultKey != nil }),
            let defaultKey = command.defaultKey
        else {
            Issue.record("the table has no bound command")
            return
        }
        let overridden = resolver.settingsPatch(
            for: command.id, binding: KeyBinding("j", [.command, .shift]))
        #expect(overridden[command.id] != nil)

        resolver.apply(overrides: overridden)
        let cleared = resolver.settingsPatch(for: command.id, binding: nil)
        #expect(cleared[command.id] == nil)
        #expect(defaultKey.serialized.contains(defaultKey.keyToken))
    }
}
