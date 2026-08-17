import FormCore
import FormDesign
import SwiftUI

/// The `#Preview` harness for a single tab. Uses `CoreStores.preview(.populated)`, which is
/// synchronous and needs no Rust build (spec 07 §6), so every pane renders on the first pass.
struct PreferencesTabPreview: View {
    let tab: PreferencesTab

    @State private var stores = CoreStores.preview(.populated)
    @State private var themeController = ThemeController()

    var body: some View {
        PreferencesSheet(
            stores: stores, themeController: themeController, tab: tab, onClose: {}
        )
        .formTheme(themeController)
    }
}
