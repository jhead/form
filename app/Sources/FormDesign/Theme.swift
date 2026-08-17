import SwiftUI

/// Design tokens. **Owner: W8** — see `docs/specs/08-design-system.md`.
///
/// No other module may define a color, font, radius, spacing or duration value. Views read
/// tokens from `@Environment(\.theme)`; nothing constructs a `Theme` except `ThemeController`
/// (and previews).
///
/// `Codable`, so alternate themes can ship as JSON without touching a view (F5.3).
public struct Theme: Sendable, Equatable, Codable, Identifiable {
    public var id: String
    public var color: ColorTokens
    public var typography: TypeTokens
    public var metrics: MetricTokens
    public var motion: MotionTokens
    public var syntax: SyntaxTokens

    public init(
        id: String,
        color: ColorTokens,
        typography: TypeTokens = TypeTokens(),
        metrics: MetricTokens = .standard,
        motion: MotionTokens = .standard,
        syntax: SyntaxTokens
    ) {
        self.id = id
        self.color = color
        self.typography = typography
        self.metrics = metrics
        self.motion = motion
        self.syntax = syntax
    }

    /// True for the dark palette. Used only for `preferredColorScheme` and for picking a
    /// material — never for branching on colors, which the tokens already handle.
    public var isDark: Bool { id == "dark" }

    public var colorScheme: ColorScheme { isDark ? .dark : .light }

    public func withTextScale(_ scale: CGFloat) -> Theme {
        var copy = self
        copy.typography = typography.withScale(scale)
        return copy
    }

    public func withHairline(_ hairline: CGFloat) -> Theme {
        var copy = self
        copy.metrics.hairline = hairline
        return copy
    }

    public static let light = Theme.lightTheme
    public static let dark = Theme.darkTheme

    /// Every theme the app ships with. `ThemeController` resolves a `ThemeMode` into one.
    public static let all: [Theme] = [.light, .dark]
}

// MARK: - Environment

private struct ThemeKey: EnvironmentKey {
    static let defaultValue = Theme.light
}

public extension EnvironmentValues {
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

public extension View {
    /// Injects a concrete theme. Prefer `.formTheme(controller)` at the app root; this
    /// exists for previews and for the crossfade in `ThemeController`.
    func theme(_ theme: Theme) -> some View {
        environment(\.theme, theme)
            .environment(\.colorScheme, theme.colorScheme)
            .tint(theme.color.accent.color)
    }
}
