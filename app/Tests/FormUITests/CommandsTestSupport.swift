import FormCore
import FormDesign
import Foundation

@testable import FormUI

/// A `CommandCenter` over the seeded mock corpus — no Rust, no window, no waiting.
@MainActor
final class CommandsHarness {
    let stores: CoreStores
    let theme: ThemeController
    let state: PreviewAppState
    let center: CommandCenter

    init(scenario: PreviewScenario = .populated, overrides: [String: String] = [:]) {
        stores = CoreStores.preview(scenario)
        theme = ThemeController(mode: .light)
        state = PreviewAppState()
        center = CommandCenter(stores: stores, theme: theme, state: state)
        center.resolver.apply(overrides: overrides)
        if let id = stores.sessions.selectedSessionId { state.showSession(id) }
    }

    var context: CommandContext { center.context }

    /// Polls until `condition` holds or the budget runs out. Debounced work in the palette
    /// and the find bar is genuinely asynchronous, and a fixed sleep either flakes or wastes
    /// time; this does neither. The budget is generous because every test in this target is
    /// `@MainActor` and the suite runs in parallel — the wait is contending for one actor.
    @discardableResult
    func wait(
        timeout: Duration = .seconds(10),
        until condition: @MainActor () -> Bool
    ) async -> Bool {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return condition()
    }
}
