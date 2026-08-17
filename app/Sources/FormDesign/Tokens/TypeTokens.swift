import AppKit
import SwiftUI

/// The three families. Serif is reserved — wordmark and display headings only (PRD §1).
public enum FontFamily: String, Sendable, Codable, CaseIterable {
    case sans
    case serif
    case mono
}

/// `Font.Weight` is not `Codable`, so weights are tokens too.
public enum FontWeightToken: String, Sendable, Codable, CaseIterable {
    case regular, medium, semibold, bold

    public var swiftUI: Font.Weight {
        switch self {
        case .regular: .regular
        case .medium: .medium
        case .semibold: .semibold
        case .bold: .bold
        }
    }

    public var appKit: NSFont.Weight {
        switch self {
        case .regular: .regular
        case .medium: .medium
        case .semibold: .semibold
        case .bold: .bold
        }
    }
}

/// One typographic style. `size` is the *base* size; the user text-size multiplier is
/// applied by `TypeTokens` before a style is handed to a view.
public struct TypeStyle: Sendable, Equatable, Codable {
    public var size: CGFloat
    public var weight: FontWeightToken
    public var family: FontFamily
    /// Multiple of `size`. 1.0 means "whatever the font's natural leading is".
    public var lineHeight: CGFloat

    public init(
        size: CGFloat,
        weight: FontWeightToken = .regular,
        family: FontFamily = .sans,
        lineHeight: CGFloat = 1.0
    ) {
        self.size = size
        self.weight = weight
        self.family = family
        self.lineHeight = lineHeight
    }

    public var font: Font {
        FontResolver.font(family: family, size: size, weight: weight)
    }

    public var nsFont: NSFont {
        FontResolver.nsFont(family: family, size: size, weight: weight)
    }

    /// Extra leading to hand to `.lineSpacing(_:)`. Zero when `lineHeight` is 1.
    public var lineSpacing: CGFloat {
        max(0, size * lineHeight - size)
    }

    public func scaled(by multiplier: CGFloat) -> TypeStyle {
        TypeStyle(size: size * multiplier, weight: weight, family: family, lineHeight: lineHeight)
    }

    public func weighted(_ weight: FontWeightToken) -> TypeStyle {
        TypeStyle(size: size, weight: weight, family: family, lineHeight: lineHeight)
    }
}

/// The type scale (spec 08 §2.2). Every accessor returns the style already multiplied by
/// `scale`, so no call site has to remember to apply it.
public struct TypeTokens: Sendable, Equatable, Codable {
    /// User text-size multiplier (`⌘+` / `⌘-` / `⌘0`), clamped to 0.85 … 1.4.
    public var scale: CGFloat {
        didSet { scale = Self.clampScale(scale) }
    }

    public var base: Scale

    public static let minimumScale: CGFloat = 0.85
    public static let maximumScale: CGFloat = 1.4

    public static func clampScale(_ value: CGFloat) -> CGFloat {
        min(maximumScale, max(minimumScale, value))
    }

    public init(base: Scale = .standard, scale: CGFloat = 1.0) {
        self.base = base
        self.scale = Self.clampScale(scale)
    }

    public var wordmark: TypeStyle { base.wordmark.scaled(by: scale) }
    public var display: TypeStyle { base.display.scaled(by: scale) }
    public var title: TypeStyle { base.title.scaled(by: scale) }
    public var heading: TypeStyle { base.heading.scaled(by: scale) }
    public var body: TypeStyle { base.body.scaled(by: scale) }
    public var bodyStrong: TypeStyle { base.bodyStrong.scaled(by: scale) }
    public var ui: TypeStyle { base.ui.scaled(by: scale) }
    public var uiMedium: TypeStyle { base.uiMedium.scaled(by: scale) }
    public var caption: TypeStyle { base.caption.scaled(by: scale) }
    public var micro: TypeStyle { base.micro.scaled(by: scale) }
    public var code: TypeStyle { base.code.scaled(by: scale) }
    public var codeInline: TypeStyle { base.codeInline.scaled(by: scale) }

    /// Mono at an arbitrary size — the Editor preferences pane lets the user pick one.
    public func mono(size: CGFloat, weight: FontWeightToken = .regular) -> TypeStyle {
        TypeStyle(size: size * scale, weight: weight, family: .mono, lineHeight: 1.5)
    }

    public func withScale(_ value: CGFloat) -> TypeTokens {
        TypeTokens(base: base, scale: value)
    }

    // Property observers do not run during `init(from:)`, so clamp explicitly — a theme
    // JSON must not be able to smuggle in an unreadable scale.
    private enum CodingKeys: String, CodingKey { case scale, base }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        base = try container.decodeIfPresent(Scale.self, forKey: .base) ?? .standard
        scale = Self.clampScale(try container.decodeIfPresent(CGFloat.self, forKey: .scale) ?? 1)
    }
}

public extension TypeTokens {
    /// Unscaled sizes. Separated from `TypeTokens` so the multiplier is applied in exactly
    /// one place and a JSON theme overrides sizes, not scaled sizes.
    struct Scale: Sendable, Equatable, Codable {
        public var wordmark: TypeStyle
        public var display: TypeStyle
        public var title: TypeStyle
        public var heading: TypeStyle
        public var body: TypeStyle
        public var bodyStrong: TypeStyle
        public var ui: TypeStyle
        public var uiMedium: TypeStyle
        public var caption: TypeStyle
        public var micro: TypeStyle
        public var code: TypeStyle
        public var codeInline: TypeStyle

        public static let standard = Scale(
            wordmark: TypeStyle(size: 20, weight: .regular, family: .serif),
            display: TypeStyle(size: 28, weight: .regular, family: .serif, lineHeight: 1.2),
            title: TypeStyle(size: 17, weight: .semibold),
            heading: TypeStyle(size: 15, weight: .semibold),
            body: TypeStyle(size: 14, weight: .regular, family: .sans, lineHeight: 1.55),
            bodyStrong: TypeStyle(size: 14, weight: .semibold, family: .sans, lineHeight: 1.55),
            ui: TypeStyle(size: 13, weight: .regular),
            uiMedium: TypeStyle(size: 13, weight: .medium),
            caption: TypeStyle(size: 12, weight: .regular),
            micro: TypeStyle(size: 11, weight: .regular),
            code: TypeStyle(size: 12.5, weight: .regular, family: .mono, lineHeight: 1.5),
            codeInline: TypeStyle(size: 13, weight: .regular, family: .mono)
        )

        public init(
            wordmark: TypeStyle,
            display: TypeStyle,
            title: TypeStyle,
            heading: TypeStyle,
            body: TypeStyle,
            bodyStrong: TypeStyle,
            ui: TypeStyle,
            uiMedium: TypeStyle,
            caption: TypeStyle,
            micro: TypeStyle,
            code: TypeStyle,
            codeInline: TypeStyle
        ) {
            self.wordmark = wordmark
            self.display = display
            self.title = title
            self.heading = heading
            self.body = body
            self.bodyStrong = bodyStrong
            self.ui = ui
            self.uiMedium = uiMedium
            self.caption = caption
            self.micro = micro
            self.code = code
            self.codeInline = codeInline
        }

        /// Every style, for tests that must cover the whole scale.
        public var all: [(name: String, style: TypeStyle)] {
            [
                ("wordmark", wordmark), ("display", display), ("title", title),
                ("heading", heading), ("body", body), ("bodyStrong", bodyStrong),
                ("ui", ui), ("uiMedium", uiMedium), ("caption", caption),
                ("micro", micro), ("code", code), ("codeInline", codeInline),
            ]
        }
    }
}

// MARK: - Family resolution

/// Resolves the three families once, honouring the fallback chains in spec 08 §2.2.
/// `.system(design:)` gives us New York and SF Mono when the OS exposes them; the named
/// fallbacks exist for the case where it does not.
enum FontResolver {
    /// New York ships with macOS and is reached through the serif *design*, not by name.
    /// If the design is unavailable we fall back to Charter, then Georgia.
    static let serifFallbackName: String? = {
        let system = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        if system.fontDescriptor.withDesign(.serif) != nil { return nil }
        return ["Charter", "Georgia"].first { NSFont(name: $0, size: NSFont.systemFontSize) != nil }
    }()

    static let monoFallbackName: String? = {
        let system = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        if system.fontDescriptor.withDesign(.monospaced) != nil { return nil }
        return ["SF Mono", "Menlo"].first { NSFont(name: $0, size: NSFont.systemFontSize) != nil }
    }()

    static func font(family: FontFamily, size: CGFloat, weight: FontWeightToken) -> Font {
        switch family {
        case .sans:
            return .system(size: size, weight: weight.swiftUI)
        case .serif:
            if let name = serifFallbackName {
                return .custom(name, fixedSize: size).weight(weight.swiftUI)
            }
            return .system(size: size, weight: weight.swiftUI, design: .serif)
        case .mono:
            if let name = monoFallbackName {
                return .custom(name, fixedSize: size).weight(weight.swiftUI)
            }
            return .system(size: size, weight: weight.swiftUI, design: .monospaced)
        }
    }

    static func nsFont(family: FontFamily, size: CGFloat, weight: FontWeightToken) -> NSFont {
        let system = NSFont.systemFont(ofSize: size, weight: weight.appKit)
        switch family {
        case .sans:
            return system
        case .serif:
            if let name = serifFallbackName, let font = NSFont(name: name, size: size) { return font }
            if let descriptor = system.fontDescriptor.withDesign(.serif) {
                return NSFont(descriptor: descriptor, size: size) ?? system
            }
            return system
        case .mono:
            if let name = monoFallbackName, let font = NSFont(name: name, size: size) { return font }
            return NSFont.monospacedSystemFont(ofSize: size, weight: weight.appKit)
        }
    }
}

// MARK: - Applying a style

public extension View {
    /// Applies font and leading together. Views never call `.font(.system(size:))`.
    func typeStyle(_ style: TypeStyle) -> some View {
        font(style.font)
            .lineSpacing(style.lineSpacing)
    }

    /// Mono/tabular figures for token counts, durations and rank numbers.
    func tabularFigures() -> some View {
        monospacedDigit()
    }
}
