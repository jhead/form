import AppKit
import FormCore
import FormDesign
import SwiftUI

public extension View {
    /// Installs everything in `Commands/` on the app's content: the global key monitor, the
    /// `⌘K` palette, the `⌘/` cheat sheet, and the `⌘F` find bar.
    ///
    /// W9 attaches this once at the root of the window. Pass `showsFindBar: false` if the
    /// shell would rather place `FindBar(center:)` itself — under the session header, where
    /// spec 14 §4 wants it — and keep the rest.
    func formCommands(_ center: CommandCenter, showsFindBar: Bool = true) -> some View {
        modifier(CommandsOverlay(center: center, showsFindBar: showsFindBar))
    }
}

/// The one place a key event enters the command system, and the one place the overlays are
/// mounted. Everything it does is bookkeeping — the behaviour lives in `CommandCenter`.
struct CommandsOverlay: ViewModifier {
    let center: CommandCenter
    let showsFindBar: Bool

    @State private var monitor: Any?

    func body(content: Content) -> some View {
        VStack(spacing: 0) {
            if showsFindBar, center.isPresented(.find) {
                FindBar(center: center)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            content
        }
        .animation(center.theme.theme.motion.animation(.fast), value: center.isPresented(.find))
        .overlay {
            if center.isPresented(.palette) {
                CommandPalette(center: center)
            }
        }
        .overlay {
            if center.isPresented(.cheatSheet) {
                CheatSheet(center: center)
            }
        }
        .environment(\.commandCenter, center)
        .onAppear(perform: startMonitor)
        .onDisappear(perform: stopMonitor)
        // `settings.shortcuts` is the user's override table; re-resolve when it moves.
        .onChange(of: center.stores.settings.settings.shortcuts) {
            center.settingsChanged()
        }
        // The sidebar's magnifier button raises the palette by setting the flag directly.
        .onChange(of: center.state.searchPresented) { center.adoptStateFlags() }
        .onChange(of: center.state.findPresented) { center.adoptStateFlags() }
        // A streaming update must not drop the current match (spec 14 §6). `FindController`
        // re-anchors by match identity, so re-running here is safe.
        .onChange(of: center.stores.chat.entries.count) { center.find.refresh() }
        .onChange(of: center.stores.chat.isStreaming) { center.find.refresh() }
        .onChange(of: center.stores.sessions.selectedSessionId) {
            // Matches belong to the session they were found in.
            if center.isPresented(.find) { center.find.refresh() }
        }
    }

    /// A local monitor sees the key before menu key-equivalent dispatch, which is what makes
    /// `Esc` and `⌘1`–`⌘9` work while a text field holds focus. Consuming the event also
    /// means the matching menu item never fires for the same keystroke, so a command runs
    /// exactly once whichever route the user took.
    private func startMonitor() {
        guard monitor == nil else { return }
        let center = center
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            // `NSEvent` is explicitly non-`Sendable`, so only the verdict crosses back out
            // of the isolated region — the event itself never leaves this closure.
            let handled = MainActor.assumeIsolated { center.handle(event: event) }
            return handled ? nil : event
        }
    }

    private func stopMonitor() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }
}

// MARK: - Previews

/// Hosts a `Commands/` view against `CoreStores.preview(.populated)` — a real seeded corpus,
/// synchronously, with no Rust build (spec 07 §6).
public struct CommandsPreviewHost<Content: View>: View {
    @State private var model = Model()
    private let content: (CommandCenter) -> Content
    private let onAppear: (CommandCenter) -> Void

    public init(
        @ViewBuilder content: @escaping (CommandCenter) -> Content,
        onAppear: @escaping (CommandCenter) -> Void = { _ in }
    ) {
        self.content = content
        self.onAppear = onAppear
    }

    public var body: some View {
        ZStack {
            model.theme.theme.color.background.ignoresSafeArea()
            content(model.center)
        }
        .frame(
            width: model.theme.theme.metrics.windowMinWidth,
            height: model.theme.theme.metrics.windowMinHeight)
        .formTheme(model.theme)
        .onAppear { onAppear(model.center) }
    }

    @MainActor
    @Observable
    final class Model {
        let stores = CoreStores.preview(.populated)
        let theme = ThemeController(mode: .light)
        let state = PreviewAppState()
        let center: CommandCenter

        init() {
            center = CommandCenter(stores: stores, theme: theme, state: state)
            state.showSession(stores.sessions.selectedSessionId ?? "")
        }
    }
}
