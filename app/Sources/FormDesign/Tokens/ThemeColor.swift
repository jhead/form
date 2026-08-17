import SwiftUI

/// A theme color: sRGB components plus alpha, stored as data so a `Theme` can round-trip
/// through JSON (F5.3). Views never build one — they read tokens off `@Environment(\.theme)`.
///
/// Conforms to `ShapeStyle` so a token drops straight into `foregroundStyle`, `fill`,
/// `background` and `stroke` without an intermediate `.color` hop.
public struct ThemeColor: Sendable, Hashable, Codable {
    public var red: Double
    public var green: Double
    public var blue: Double
    public var alpha: Double

    /// Channels are quantised to 8 bits on the way in. That is the precision the display
    /// has, and it makes the hex representation lossless — so a theme survives a JSON
    /// round-trip byte for byte, and two colors that render identically compare equal.
    public init(red: Double, green: Double, blue: Double, alpha: Double = 1) {
        self.red = red.quantised8Bit
        self.green = green.quantised8Bit
        self.blue = blue.quantised8Bit
        self.alpha = alpha.quantised8Bit
    }

    /// `"#RRGGBB"`, `"#RRGGBBAA"`, `"#RGB"` — with or without the leading `#`.
    /// Invalid input resolves to opaque magenta so a mistake is loud in a preview rather
    /// than silently invisible; `Codable` decoding throws instead.
    public init(hex: String, alpha: Double = 1) {
        guard let parsed = Self.parse(hex: hex) else {
            self.init(red: 1, green: 0, blue: 1, alpha: 1)
            return
        }
        self.init(red: parsed.0, green: parsed.1, blue: parsed.2, alpha: parsed.3 * alpha)
    }

    // MARK: SwiftUI

    public var color: Color {
        Color(.sRGB, red: red, green: green, blue: blue, opacity: alpha)
    }

    public var nsColor: NSColor {
        NSColor(srgbRed: red, green: green, blue: blue, alpha: alpha)
    }

    // MARK: Derivation

    public func opacity(_ value: Double) -> ThemeColor {
        ThemeColor(red: red, green: green, blue: blue, alpha: alpha * value.clamped01)
    }

    /// Linear blend in sRGB space. Used for derived states (hover tints, chart fills).
    public func mixed(with other: ThemeColor, amount: Double) -> ThemeColor {
        let t = amount.clamped01
        return ThemeColor(
            red: red + (other.red - red) * t,
            green: green + (other.green - green) * t,
            blue: blue + (other.blue - blue) * t,
            alpha: alpha + (other.alpha - alpha) * t
        )
    }

    // MARK: Contrast

    /// WCAG 2.1 relative luminance. Alpha is ignored — composite first if it matters.
    public var relativeLuminance: Double {
        func linear(_ c: Double) -> Double {
            c <= 0.040_45 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    /// WCAG 2.1 contrast ratio, 1.0 … 21.0. Order-independent.
    public func contrastRatio(against other: ThemeColor) -> Double {
        let a = relativeLuminance
        let b = other.relativeLuminance
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    /// Composites `self` over `background`, so a translucent token can be measured.
    public func composited(over background: ThemeColor) -> ThemeColor {
        background.mixed(with: ThemeColor(red: red, green: green, blue: blue), amount: alpha)
    }

    // MARK: Hex

    public var hexString: String {
        let r = Int((red * 255).rounded())
        let g = Int((green * 255).rounded())
        let b = Int((blue * 255).rounded())
        if alpha >= 1 {
            return String(format: "#%02X%02X%02X", r, g, b)
        }
        return String(format: "#%02X%02X%02X%02X", r, g, b, Int((alpha * 255).rounded()))
    }

    private static func parse(hex: String) -> (Double, Double, Double, Double)? {
        var s = hex.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.allSatisfy(\.isHexDigit) else { return nil }
        func channel(_ i: Int, width: Int) -> Double {
            let start = s.index(s.startIndex, offsetBy: i * width)
            let end = s.index(start, offsetBy: width)
            let raw = String(s[start ..< end])
            let value = UInt32(raw, radix: 16) ?? 0
            return Double(value) / (width == 1 ? 15.0 : 255.0)
        }
        switch s.count {
        case 3: return (channel(0, width: 1), channel(1, width: 1), channel(2, width: 1), 1)
        case 6: return (channel(0, width: 2), channel(1, width: 2), channel(2, width: 2), 1)
        case 8: return (channel(0, width: 2), channel(1, width: 2), channel(2, width: 2), channel(3, width: 2))
        default: return nil
        }
    }

    // MARK: Codable — hex strings, because that is what a theme JSON should look like.

    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(String.self)
        guard let parsed = Self.parse(hex: raw) else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "not a hex color: \(raw)"
            )
        }
        self.init(red: parsed.0, green: parsed.1, blue: parsed.2, alpha: parsed.3)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(hexString)
    }
}

extension ThemeColor: ShapeStyle {
    public func resolve(in environment: EnvironmentValues) -> Color { color }
}

extension ThemeColor: View {
    public var body: some View { color }
}

private extension Double {
    var clamped01: Double { Swift.min(1, Swift.max(0, self)) }
    var quantised8Bit: Double { (clamped01 * 255).rounded() / 255 }
}
