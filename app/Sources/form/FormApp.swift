import SwiftUI
import FormUI

/// **Owner: W9.** Window, menus, and the single place the core is constructed.
@main
struct FormApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
        }
        .windowStyle(.hiddenTitleBar)
        // TODO(W9/W14): commands from the shortcut table, window sizing, restoration.
    }
}
