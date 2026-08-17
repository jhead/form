import AppKit
import Observation
import SwiftUI

/// Persisted in core settings (spec 04). `system` follows the OS live (F5.1).
public enum ThemeMode: String, Sendable, Codable, CaseIterable, Identifiable {
    case light, dark, system

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .light: "Light"
        case .dark: "Dark"
        case .system: "System"
        }
    }

    public var systemImage: String {
        switch self {
        case .light: "sun.max"
        case .dark: "moon"
        case .system: "circle.lefthalf.filled"
        }
    }

    /// `⌘⇧D` cycles light ⇄ dark and leaves `system` for the picker.
    @MainActor
    public var toggled: ThemeMode {
        switch self {
        case .light: .dark
        case .dark: .light
        case .system: ThemeController.systemIsDark() ? .light : .dark
        }
    }
}

/// Resolves `ThemeMode` + the system appearance into a concrete `Theme`, and republishes
/// when either the OS appearance, the reduce-motion setting, or the screen changes.
///
/// Mutators are methods rather than settable properties: `@Observable` does not support
/// property observers, and every change has to rebuild the resolved theme.
@MainActor
@Observable
public final class ThemeController {
    public private(set) var mode: ThemeMode
    public private(set) var textScale: CGFloat
    public private(set) var theme: Theme
    /// Mirrors `NSWorkspace.accessibilityDisplayShouldReduceMotion` so views re-render when
    /// the user changes it. Views must still build animations via `theme.motion`.
    public private(set) var reduceMotion: Bool

    @ObservationIgnored private var appearanceObservation: NSKeyValueObservation?
    @ObservationIgnored private var accessibilityObserver: (any NSObjectProtocol)?
    @ObservationIgnored private var screenObserver: (any NSObjectProtocol)?

    public init(mode: ThemeMode = .system, textScale: CGFloat = 1.0) {
        self.mode = mode
        self.textScale = TypeTokens.clampScale(textScale)
        theme = Self.resolve(mode: mode, textScale: textScale)
        reduceMotion = ReduceMotion.isEnabled
        startObserving()
    }

    // MARK: Mutation

    public func setMode(_ newMode: ThemeMode) {
        guard newMode != mode else { return }
        mode = newMode
        refresh()
    }

    public func setTextScale(_ newScale: CGFloat) {
        let clamped = TypeTokens.clampScale(newScale)
        guard clamped != textScale else { return }
        textScale = clamped
        refresh()
    }

    /// `⌘+` / `⌘-`. Steps through the ladder rather than by a fixed delta so the ends land
    /// exactly on the clamp values.
    public func stepTextScale(_ direction: Int) {
        let ladder: [CGFloat] = [0.85, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4]
        let current = ladder.enumerated().min { a, b in
            abs(a.element - textScale) < abs(b.element - textScale)
        }?.offset ?? 2
        let next = min(ladder.count - 1, max(0, current + direction))
        setTextScale(ladder[next])
    }

    /// `⌘0`.
    public func resetTextScale() { setTextScale(1.0) }

    /// `⌘⇧D`.
    public func toggleAppearance() { setMode(mode.toggled) }

    /// Rebuilds the resolved theme. Safe to call redundantly — it assigns only on change,
    /// so it will not churn `@Observable` dependents.
    public func refresh() {
        let resolved = Self.resolve(mode: mode, textScale: textScale)
        if resolved != theme { theme = resolved }
        let reduced = ReduceMotion.isEnabled
        if reduced != reduceMotion { reduceMotion = reduced }
    }

    // MARK: Resolution

    public static func resolve(mode: ThemeMode, textScale: CGFloat) -> Theme {
        let base: Theme = switch mode {
        case .light: .light
        case .dark: .dark
        case .system: systemIsDark() ? .dark : .light
        }
        return base
            .withTextScale(textScale)
            .withHairline(MetricTokens.currentHairline())
    }

    public static func systemIsDark() -> Bool {
        let appearance = NSApp?.effectiveAppearance ?? NSAppearance.currentDrawing()
        return appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
    }

    // MARK: Observation

    private func startObserving() {
        // The OS appearance. KVO on NSApplication is the only signal that fires for both
        // the system Appearance switch and an app-level override.
        if let app = NSApp {
            appearanceObservation = app.observe(\.effectiveAppearance, options: [.new]) { [weak self] _, _ in
                Task { @MainActor in self?.refresh() }
            }
        }

        accessibilityObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }

        // Moving the window to a non-Retina display changes `hairline`.
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }
}

// MARK: - Root injection

public extension View {
    /// Injects the resolved theme and crossfades over `motion.normal` when it changes.
    /// The crossfade is an animation on the theme value only — it does not re-identify any
    /// view, so scroll position and first responder survive (F5.4).
    func formTheme(_ controller: ThemeController) -> some View {
        modifier(FormThemeModifier(controller: controller))
    }
}

private struct FormThemeModifier: ViewModifier {
    let controller: ThemeController

    func body(content: Content) -> some View {
        content
            .theme(controller.theme)
            .environment(controller)
            .animation(controller.theme.motion.animation(.normal), value: controller.theme)
    }
}
