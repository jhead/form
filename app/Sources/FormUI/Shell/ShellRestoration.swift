import AppKit
import Foundation
import SwiftUI

/// Window-local UI state that has no home in the core's settings document.
///
/// Sidebar width and collapse persist through `SettingsStore` because the settings schema
/// names them (spec 04) and the preferences surface edits them. The last route and the
/// `Ungrouped` disclosure have no such key — and inventing one in Swift would put the app
/// ahead of the core's schema — so they live in `UserDefaults` alongside the window frame,
/// which AppKit already autosaves there. See the W9 report.
enum ShellRestoration {
    private static let routeKey = "shell.lastRoute"
    private static let ungroupedKey = "shell.ungroupedCollapsed"

    static var route: AppRoute? {
        get { UserDefaults.standard.string(forKey: routeKey).flatMap(AppRoute.init(persisted:)) }
        set {
            guard let newValue else {
                UserDefaults.standard.removeObject(forKey: routeKey)
                return
            }
            UserDefaults.standard.set(newValue.persisted, forKey: routeKey)
        }
    }

    static var ungroupedCollapsed: Bool {
        get { UserDefaults.standard.bool(forKey: ungroupedKey) }
        set { UserDefaults.standard.set(newValue, forKey: ungroupedKey) }
    }
}

/// Applies the window settings SwiftUI has no modifier for: the autosaved frame and the
/// minimum size (spec 09 §1).
struct WindowConfigurator: NSViewRepresentable {
    let autosaveName: String
    let minSize: CGSize

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        // The window is not attached yet at `makeNSView` time.
        DispatchQueue.main.async { configure(view.window) }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        configure(nsView.window)
    }

    private func configure(_ window: NSWindow?) {
        guard let window else { return }
        window.minSize = minSize
        if window.frameAutosaveName != autosaveName {
            window.setFrameAutosaveName(autosaveName)
        }
    }
}

/// Reports the sidebar's laid-out width so a user's drag can be persisted. A preference is
/// the safe channel for this — writing observable state from inside a `GeometryReader` would
/// mutate during view update.
struct SidebarWidthKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        let next = nextValue()
        if next > 0 { value = next }
    }
}

extension View {
    func measuringSidebarWidth() -> some View {
        background(
            GeometryReader { proxy in
                Color.clear  // FormDesign-allow: a measurement probe, never drawn
                    .preference(key: SidebarWidthKey.self, value: proxy.size.width)
            }
        )
    }
}
