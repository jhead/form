import AppKit
import FormCore
import FormDesign
import Testing

@testable import FormUI

/// The global key handler, the `Esc` responder chain, and the handful of commands whose
/// behaviour is more than a one-liner.
@MainActor
struct CommandCenterTests {
    // MARK: - Key handling

    @Test("an event resolves to its command and runs it")
    func handlesABoundKey() async throws {
        let harness = CommandsHarness()
        var opened = false
        harness.center.hooks.chooseWorkspaceFolder = { opened = true }

        let event = try #require(KeyBindingTests.keyDown("f", [.command, .shift]))
        #expect(harness.center.handle(event: event))
        await harness.wait { opened }
        #expect(opened)
    }

    @Test("an unbound key is passed through")
    func ignoresUnboundKeys() throws {
        let harness = CommandsHarness()
        let event = try #require(KeyBindingTests.keyDown("j", [.command, .control, .option]))
        #expect(!harness.center.handle(event: event))
    }

    @Test("plain typing is never swallowed")
    func ignoresPlainCharacters() throws {
        let harness = CommandsHarness()
        for character in ["a", "z", "1", " "] {
            let event = try #require(KeyBindingTests.keyDown(character, []))
            #expect(!harness.center.handle(event: event), "swallowed a plain \(character)")
        }
    }

    @Test("a disabled command swallows its key without running")
    func disabledCommandsSwallow() throws {
        let harness = CommandsHarness()
        #expect(!harness.center.find.hasMatches)
        let event = try #require(KeyBindingTests.keyDown("g", [.command]))
        // ⌘G is consumed so it cannot fall through to a text view's own find-next.
        #expect(harness.center.handle(event: event))
        #expect(harness.center.find.currentIndex == 0)
    }

    @Test("an override reroutes the key")
    func overrideReroutes() async throws {
        let harness = CommandsHarness(overrides: ["help.cheatSheet": "cmd+t"])
        let event = try #require(KeyBindingTests.keyDown("t", [.command]))
        #expect(harness.center.handle(event: event))
        // `handle` dispatches the command asynchronously, as the key monitor must.
        await harness.wait { harness.center.isPresented(.cheatSheet) }
        #expect(harness.center.isPresented(.cheatSheet))

        let old = try #require(KeyBindingTests.keyDown("/", [.command]))
        #expect(!harness.center.handle(event: old), "the default should have been released")
    }

    @Test("a registered interceptor gets first refusal")
    func interceptorsWinFirst() throws {
        let harness = CommandsHarness()
        var intercepted = 0
        harness.center.registerKeyInterceptor(id: "test") { _ in
            intercepted += 1
            return true
        }
        let event = try #require(KeyBindingTests.keyDown("n", [.command]))
        #expect(harness.center.handle(event: event))
        #expect(intercepted == 1)

        harness.center.unregisterKeyInterceptor(id: "test")
        #expect(harness.center.handle(event: event))
        #expect(intercepted == 1)
    }

    // MARK: - Esc

    @Test("the responder chain runs overlay, then streaming, then composer focus")
    func escapeChainIsOrdered() {
        let harness = CommandsHarness()
        #expect(
            harness.center.escapeChain.responderIDs
                == ["commands.overlay", "commands.stopStreaming", "commands.composerFocus"])
    }

    @Test("Esc dismisses the topmost overlay first")
    func escapeDismissesTopmostOverlay() async {
        let harness = CommandsHarness(scenario: .streaming)
        var composerCleared = false
        harness.center.hooks.clearComposerFocus = {
            composerCleared = true
            return true
        }

        harness.center.togglePalette()
        harness.center.toggleCheatSheet()
        #expect(harness.center.topmostOverlay == .cheatSheet)

        #expect(await harness.center.handleEscape())
        #expect(harness.center.topmostOverlay == .palette, "the newer overlay goes first")

        #expect(await harness.center.handleEscape())
        #expect(harness.center.overlays.isEmpty)
        #expect(!composerCleared, "focus is the last resort, not the first")
    }

    @Test("Esc stops streaming when no overlay is open")
    func escapeStopsStreaming() async {
        let harness = CommandsHarness(scenario: .streaming)
        var composerCleared = false
        harness.center.hooks.clearComposerFocus = {
            composerCleared = true
            return true
        }
        #expect(harness.stores.chat.isStreaming)
        #expect(await harness.center.handleEscape())
        #expect(!composerCleared, "streaming outranks composer focus")
    }

    @Test("Esc clears composer focus when there is nothing else to do")
    func escapeClearsComposerFocus() async {
        let harness = CommandsHarness()
        var composerCleared = false
        harness.center.hooks.clearComposerFocus = {
            composerCleared = true
            return true
        }
        #expect(!harness.stores.chat.isStreaming)
        #expect(await harness.center.handleEscape())
        #expect(composerCleared)
    }

    @Test("Esc is not consumed when nothing wants it")
    func escapeFallsThrough() async {
        let harness = CommandsHarness()
        #expect(!(await harness.center.handleEscape()))
    }

    // MARK: - Overlays and W9's flags

    @Test("presenting the palette mirrors into AppState, and back")
    func overlayFlagsMirror() {
        let harness = CommandsHarness()
        harness.center.togglePalette()
        #expect(harness.state.searchPresented)

        harness.center.dismiss(.palette)
        #expect(!harness.state.searchPresented)

        // The sidebar's magnifier sets the flag directly; the centre adopts it.
        harness.state.searchPresented = true
        harness.center.adoptStateFlags()
        #expect(harness.center.isPresented(.palette))
    }

    // MARK: - Individual commands

    @Test("⌘+ and ⌘- walk the text-size ladder, ⌘0 resets")
    func textSizeLadder() async {
        let harness = CommandsHarness()
        let ladder = ThemeController.textScaleLadder
        #expect(harness.theme.textScale == 1.0)

        await harness.center.run(id: "view.textSizeIncrease")
        let up = ladder[ladder.firstIndex(of: 1.0).map { $0 + 1 } ?? 0]
        #expect(harness.theme.textScale == up)
        #expect(harness.stores.settings.settings.appearance.textSizeMultiplier == Double(up))

        await harness.center.run(id: "view.textSizeDecrease")
        #expect(harness.theme.textScale == 1.0)

        await harness.center.run(id: "view.textSizeIncrease")
        await harness.center.run(id: "view.textSizeIncrease")
        await harness.center.run(id: "view.textSizeReset")
        #expect(harness.theme.textScale == 1.0)
    }

    @Test("zooming stops at the ends of the ladder")
    func textSizeClamps() async {
        let harness = CommandsHarness()
        for _ in 0..<12 { await harness.center.run(id: "view.textSizeIncrease") }
        #expect(harness.theme.textScale == ThemeController.textScaleLadder.last)
        for _ in 0..<12 { await harness.center.run(id: "view.textSizeDecrease") }
        #expect(harness.theme.textScale == ThemeController.textScaleLadder.first)
    }

    @Test("⌘[ and ⌘] traverse the route stack, not the sidebar order")
    func historyTraversal() async {
        let harness = CommandsHarness()
        let ordered = harness.stores.sessions.ordered
        try? #require(ordered.count >= 3)

        harness.state.showSession(ordered[2].id)
        harness.state.showHome()
        harness.state.showSession(ordered[0].id)

        await harness.center.run(id: "nav.back")
        #expect(harness.state.isShowingHome, "back follows where the user has been")

        await harness.center.run(id: "nav.back")
        #expect(harness.state.currentSessionId == ordered[2].id)

        await harness.center.run(id: "nav.forward")
        #expect(harness.state.isShowingHome)
    }

    @Test("back and forward are disabled at the ends of the stack")
    func historyEnablement() {
        let harness = CommandsHarness()
        let state = PreviewAppState()
        let center = CommandCenter(stores: harness.stores, theme: harness.theme, state: state)
        let back = AppCommands.command(id: "nav.back")
        let forward = AppCommands.command(id: "nav.forward")
        #expect(back?.isEnabled(center.context) == false)
        #expect(forward?.isEnabled(center.context) == false)

        state.showSession("ses_health_check")
        #expect(back?.isEnabled(center.context) == true)
        #expect(forward?.isEnabled(center.context) == false)
    }

    /// The rank a user sees comes from `SidebarOrder`, which orders by the core's dense
    /// manual `index` so a dragged session stays put. `SessionStore.ordered` sorts by
    /// pinned-then-`updatedAt` and disagrees; a numbered jump must follow the rows on screen.
    @Test("⌘1–⌘9 jump to the sidebar's flattened visible order")
    func rankJumps() async throws {
        let harness = CommandsHarness()
        let visible = SidebarOrder.visibleSessions(in: harness.stores.sessions)
        try #require(visible.count >= 2)

        await harness.center.run(id: "nav.session2")
        #expect(harness.stores.sessions.selectedSessionId == visible[1].id)
        #expect(harness.state.currentSessionId == visible[1].id)

        // A rank past the end of the list is disabled rather than crashing.
        let ninth = AppCommands.command(id: "nav.session9")
        #expect(ninth?.isEnabled(harness.context) == (visible.count >= 9))
    }

    @Test("a collapsed group's rows are not numbered")
    func rankSkipsCollapsedGroups() async throws {
        let harness = CommandsHarness()
        let store = harness.stores.sessions
        let before = SidebarOrder.visibleSessions(in: store)
        let firstGroup = try #require(store.groups.first { !SidebarOrder.sessions(in: $0, store: store).isEmpty })

        try await store.setCollapsed(firstGroup.id, true)
        let after = SidebarOrder.visibleSessions(in: store)
        #expect(after.count < before.count)

        await harness.center.run(id: "nav.session1")
        #expect(harness.stores.sessions.selectedSessionId == after.first?.id)
    }

    @Test("⌘\\ toggles the sidebar and persists it")
    func sidebarToggle() async {
        let harness = CommandsHarness()
        #expect(!harness.state.sidebarCollapsed)
        await harness.center.run(id: "view.toggleSidebar")
        #expect(harness.state.sidebarCollapsed)
        #expect(harness.stores.settings.settings.appearance.sidebarCollapsed)
    }

    @Test("⌘⇧D toggles appearance and writes it back to settings")
    func appearanceToggle() async {
        let harness = CommandsHarness()
        #expect(harness.theme.mode == .light)
        await harness.center.run(id: "view.toggleAppearance")
        #expect(harness.theme.mode == .dark)
        #expect(harness.stores.settings.settings.appearance.themeMode == FormCore.ThemeMode.dark)
    }

    @Test("⌘↩ asks the composer to send")
    func sendGoesThroughTheComposer() async {
        let harness = CommandsHarness()
        var sent = 0
        harness.center.hooks.submitComposer = { sent += 1 }
        await harness.center.run(id: "chat.send")
        #expect(sent == 1)
    }

    @Test("⌘⇧C copies the last assistant response")
    func copyLastResponse() async throws {
        let harness = CommandsHarness()
        let expected = try #require(harness.stores.chat.lastAssistantText)
        await harness.center.run(id: "chat.copyLast")
        #expect(NSPasteboard.general.string(forType: .string) == expected)
    }

    @Test("commands that need a session are disabled without one")
    func sessionScopedEnablement() {
        let harness = CommandsHarness(scenario: .empty)
        harness.stores.sessions.selectedSessionId = nil
        for id in ["session.archive", "find.open", "chat.send", "session.newFromCurrent"] {
            #expect(
                AppCommands.command(id: id)?.isEnabled(harness.context) == false,
                "\(id) should be disabled with no session")
        }
    }

    @Test("running a command counts toward the palette's most-used list")
    func usageIsCounted() async {
        let harness = CommandsHarness()
        #expect(harness.center.usageCount("nav.home") == 0)
        await harness.center.run(id: "nav.home")
        #expect(harness.center.usageCount("nav.home") == 1)
        #expect(harness.center.suggestedCommands(limit: 3).first?.id == "nav.home")
    }
}
