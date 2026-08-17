import SwiftUI

/// Design tokens. **Owner: W8** — see `docs/specs/08-design-system.md`.
/// No other module may define a color, font, radius, spacing or duration value.
/// TODO(W8): the full token set, both themes, primitives, and the contrast test.
public struct Theme: Sendable, Equatable {
    public var id: String
    public init(id: String) { self.id = id }

    public static let light = Theme(id: "light")
    public static let dark = Theme(id: "dark")
}

private struct ThemeKey: EnvironmentKey {
    static let defaultValue = Theme.light
}

public extension EnvironmentValues {
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

/// The wordmark: `form`, always lowercase, always serif.
public struct Wordmark: View {
    private let size: CGFloat
    public init(size: CGFloat = 20) { self.size = size }
    public var body: some View {
        Text(verbatim: "form").font(.system(size: size, design: .serif))
    }
}
