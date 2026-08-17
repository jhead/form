import AppKit
import FormCore
import FormDesign
import Observation
import SwiftUI

/// An overlay that participates in the `Esc` chain. Other workstreams may push their own —
/// W13's preferences sheet, for instance — so `Esc` dismisses whatever is genuinely on top.
public struct OverlayID: Hashable, Sendable, Identifiable {
    public let rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
    public var id: String { rawValue }

    public static let palette = OverlayID("palette")
    public static let find = OverlayID("find")
    public static let cheatSheet = OverlayID("cheatSheet")
}

/// The one object the rest of the app talks to for shortcuts, the palette, find and the
/// cheat sheet. W9 builds it at the root and injects it; everything in `Commands/` reads it
/// from the environment.
@MainActor
@Observable
public final class CommandCenter {
    public let stores: CoreStores
    public let theme: ThemeController
    public let state: any CommandAppState
    public let resolver: ShortcutResolver
    public let escapeChain = EscapeResponderChain()

    // Built on first use rather than in `init`: both hold the centre, and a stored property
    // cannot reference `self` before every stored property exists. They are created exactly
    // once and never replaced, so callers can hold onto them.
    @ObservationIgnored private var storedPalette: PaletteModel?
    @ObservationIgnored private var storedFind: FindController?

    public var palette: PaletteModel {
        if let storedPalette { return storedPalette }
        let model = PaletteModel(center: self)
        storedPalette = model
        return model
    }

    public var find: FindController {
        if let storedFind { return storedFind }
        let controller = FindController(center: self)
        storedFind = controller
        return controller
    }

    /// Filled in by W10/W13 at startup — see `CommandHooks`.
    public var hooks = CommandHooks()

    /// Presentation order, so `Esc` dismisses the topmost rather than a fixed one.
    public private(set) var overlays: [OverlayID] = []

    /// How often each command has been run this launch. Feeds the palette's empty-query
    /// "most used" list; deliberately in-memory, because a usage counter is not worth a
    /// settings round trip on every keystroke.
    private var usageCounts: [String: Int] = [:]

    public init(
        stores: CoreStores,
        theme: ThemeController,
        state: any CommandAppState,
        commands: [AppCommand] = AppCommands.all
    ) {
        self.stores = stores
        self.theme = theme
        self.state = state
        resolver = ShortcutResolver(
            commands: commands, overrides: stores.settings.settings.shortcuts ?? [:])
        registerBuiltInEscapeResponders()
    }

    public var context: CommandContext {
        CommandContext(stores: stores, theme: theme, state: state, center: self)
    }

    // MARK: - Running commands

    public func isEnabled(_ command: AppCommand) -> Bool {
        command.isEnabled(context)
    }

    public func perform(_ command: AppCommand) {
        guard command.isEnabled(context) else { return }
        usageCounts[command.id, default: 0] += 1
        let context = context
        Task { await command.perform(context) }
    }

    public func perform(id: String) {
        guard let command = AppCommands.command(id: id) else { return }
        perform(command)
    }

    /// Runs a command and waits for it — the path tests take, so an assertion does not race
    /// the action.
    public func run(_ command: AppCommand) async {
        guard command.isEnabled(context) else { return }
        usageCounts[command.id, default: 0] += 1
        await command.perform(context)
    }

    public func run(id: String) async {
        guard let command = AppCommands.command(id: id) else { return }
        await run(command)
    }

    public func usageCount(_ id: String) -> Int { usageCounts[id] ?? 0 }

    /// Commands for an empty palette query: what has been used, then a curated starting set
    /// so a fresh launch is not an empty list.
    public func suggestedCommands(limit: Int) -> [AppCommand] {
        let ranked = AppCommands.all
            .filter { usageCount($0.id) > 0 }
            .sorted { usageCount($0.id) > usageCount($1.id) }
        let seeds = Self.defaultSuggestions.compactMap { AppCommands.command(id: $0) }
        var seen = Set<String>()
        return (ranked + seeds)
            .filter { seen.insert($0.id).inserted }
            .prefix(limit)
            .map { $0 }
    }

    private static let defaultSuggestions = [
        "session.new", "nav.home", "find.open", "view.toggleAppearance",
        "app.preferences", "help.cheatSheet",
    ]

    // MARK: - Key handling

    /// First refusal on a key event, for a presented overlay. Most recently registered wins,
    /// so the palette's `⌘⏎` beats the table's `⌘↩ Send` while the palette is up.
    public typealias KeyInterceptor = @MainActor (NSEvent) -> Bool

    @ObservationIgnored private var interceptors: [(id: String, handle: KeyInterceptor)] = []

    public func registerKeyInterceptor(id: String, handle: @escaping KeyInterceptor) {
        interceptors.removeAll { $0.id == id }
        interceptors.append((id, handle))
    }

    public func unregisterKeyInterceptor(id: String) {
        interceptors.removeAll { $0.id == id }
    }

    /// The global key handler (spec 14 §1). Returns `true` when the event was consumed.
    ///
    /// This runs ahead of menu key-equivalent dispatch, which is what lets `Esc` and
    /// `⌘1`–`⌘9` work while a text field has focus. Because it consumes the event, a menu
    /// item never fires for the same keystroke — the action runs exactly once either way.
    @discardableResult
    public func handle(event: NSEvent) -> Bool {
        guard event.type == .keyDown else { return false }
        for interceptor in interceptors.reversed() where interceptor.handle(event) {
            return true
        }
        guard let command = resolver.command(for: event) else { return false }
        guard command.isEnabled(context) else {
            // A disabled command still swallows its key: letting `⌘G` fall through to a
            // text view's own "find next" when there is nothing to find is worse than doing
            // nothing.
            return true
        }
        perform(command)
        return true
    }

    // MARK: - Overlays

    public func isPresented(_ overlay: OverlayID) -> Bool { overlays.contains(overlay) }
    public var topmostOverlay: OverlayID? { overlays.last }

    public func present(_ overlay: OverlayID) {
        overlays.removeAll { $0 == overlay }
        overlays.append(overlay)
        mirrorToState()
    }

    public func dismiss(_ overlay: OverlayID) {
        guard overlays.contains(overlay) else { return }
        overlays.removeAll { $0 == overlay }
        if overlay == .find { find.close() }
        if overlay == .palette { palette.reset() }
        mirrorToState()
    }

    @discardableResult
    public func dismissTopmostOverlay() -> Bool {
        guard let top = overlays.last else { return false }
        dismiss(top)
        return true
    }

    public func togglePalette() {
        if isPresented(.palette) {
            dismiss(.palette)
        } else {
            palette.begin()
            present(.palette)
        }
    }

    public func openFind(seed: String?) {
        find.open(seed: seed)
        present(.find)
    }

    public func toggleCheatSheet() {
        isPresented(.cheatSheet) ? dismiss(.cheatSheet) : present(.cheatSheet)
    }

    /// W9's `AppState` carries the two presentation flags (spec 09 §5). The overlay stack is
    /// the truth — it has to be, because `Esc` needs an order — so the flags are written
    /// through here, and `adopt(from:)` picks up a change made from the other side.
    private func mirrorToState() {
        let palettePresented = isPresented(.palette)
        if state.searchPresented != palettePresented { state.searchPresented = palettePresented }
        let findShown = isPresented(.find)
        if state.findPresented != findShown { state.findPresented = findShown }
    }

    /// Reconciles a flag someone else flipped — the sidebar's magnifier button, say.
    public func adoptStateFlags() {
        if state.searchPresented, !isPresented(.palette) {
            palette.begin()
            present(.palette)
        } else if !state.searchPresented, isPresented(.palette) {
            dismiss(.palette)
        }
        if state.findPresented, !isPresented(.find) {
            openFind(seed: hooks.selectedText?())
        } else if !state.findPresented, isPresented(.find) {
            dismiss(.find)
        }
    }

    // MARK: - Escape

    private func registerBuiltInEscapeResponders() {
        escapeChain.register(id: "commands.overlay", order: EscapeResponder.Order.overlay) {
            [weak self] in
            self?.dismissTopmostOverlay() ?? false
        }
        escapeChain.register(
            id: "commands.stopStreaming", order: EscapeResponder.Order.stopStreaming
        ) { [weak self] in
            guard let self, stores.chat.isStreaming else { return false }
            try? await stores.chat.abort()
            return true
        }
        escapeChain.register(
            id: "commands.composerFocus", order: EscapeResponder.Order.composerFocus
        ) { [weak self] in
            self?.hooks.clearComposerFocus?() ?? false
        }
    }

    @discardableResult
    public func handleEscape() async -> Bool {
        await escapeChain.handle()
    }

    // MARK: - Navigation

    /// After `⌘[` / `⌘]` moved the route, bring the stores along: select the session the
    /// route now points at and load its transcript.
    public func syncSelection() async {
        if let id = state.currentSessionId {
            await stores.select(id)
        }
    }

    /// Opens a session from the palette, pushing onto the route stack so `⌘[` comes back.
    public func open(sessionId: String) async {
        state.showSession(sessionId)
        await stores.select(sessionId)
    }

    // MARK: - Settings

    /// Re-resolves bindings when `settings.shortcuts` changes. The overlay modifier wires
    /// this to an `onChange`; nothing else has to remember to call it.
    public func settingsChanged() {
        resolver.apply(overrides: stores.settings.settings.shortcuts)
    }
}

// MARK: - Environment

private struct CommandCenterKey: EnvironmentKey {
    static let defaultValue: CommandCenter? = nil
}

public extension EnvironmentValues {
    /// `nil` outside the app root; every view in `Commands/` treats that as "do nothing".
    var commandCenter: CommandCenter? {
        get { self[CommandCenterKey.self] }
        set { self[CommandCenterKey.self] = newValue }
    }
}
