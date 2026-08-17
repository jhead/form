import Foundation
import Testing

@testable import FormDesign

struct ThemeTests {
    @Test("themes are distinct")
    func themesAreDistinct() {
        #expect(Theme.light != Theme.dark)
        #expect(Theme.light.id == "light")
        #expect(Theme.dark.id == "dark")
        #expect(Theme.dark.isDark)
        #expect(!Theme.light.isDark)
    }

    /// F5.3 — alternate themes ship as JSON later, so the value has to survive the trip.
    @Test("a theme round-trips through JSON", arguments: ThemeKind.allCases)
    func codableRoundTrip(kind: ThemeKind) throws {
        let encoder = JSONEncoder()
        let data = try encoder.encode(kind.theme)
        let decoded = try JSONDecoder().decode(Theme.self, from: data)
        #expect(decoded == kind.theme)
    }

    @Test("colors encode as hex, not as component objects")
    func colorsEncodeAsHex() throws {
        let data = try JSONEncoder().encode(Theme.light.color.accent)
        #expect(String(data: data, encoding: .utf8) == "\"#C15F3C\"")
    }

    @Test("hex parsing handles the three widths and rejects garbage")
    func hexParsing() throws {
        #expect(ThemeColor(hex: "#FFF") == ThemeColor(hex: "#FFFFFF"))
        #expect(ThemeColor(hex: "C15F3C") == ThemeColor(hex: "#C15F3C"))
        #expect(ThemeColor(hex: "#00000080").alpha == 128.0 / 255.0)
        // Decoding is strict where the convenience initializer is forgiving.
        #expect(throws: DecodingError.self) {
            _ = try JSONDecoder().decode(ThemeColor.self, from: Data(#""not-a-color""#.utf8))
        }
    }

    @Test("contrast math matches the WCAG reference values")
    func contrastMath() {
        let white = ThemeColor(hex: "#FFFFFF")
        let black = ThemeColor(hex: "#000000")
        #expect(abs(white.contrastRatio(against: black) - 21.0) < 0.001)
        #expect(abs(white.contrastRatio(against: white) - 1.0) < 0.001)
        // Order does not matter.
        #expect(white.contrastRatio(against: black) == black.contrastRatio(against: white))
        // #777777 on white is the canonical 4.48:1 near-miss.
        let gray = ThemeColor(hex: "#777777")
        #expect(abs(gray.contrastRatio(against: white) - 4.48) < 0.02)
    }

    @Test("compositing a translucent token gives a measurable color")
    func compositing() {
        let scrim = Theme.light.color.overlay
        let composited = scrim.composited(over: Theme.light.color.background)
        #expect(composited.alpha == 1.0)
        #expect(composited.relativeLuminance < Theme.light.color.background.relativeLuminance)
    }

    @Test("every token key is defined in both themes")
    func bothThemesDefineEveryKey() throws {
        // Encoding to a dictionary is the only way to assert "every key", since a missing
        // token would be a compile error but a *wrong* one (a token copied from the other
        // theme) would not.
        for kind in ThemeKind.allCases {
            let data = try JSONEncoder().encode(kind.theme.color)
            let object = try #require(
                try JSONSerialization.jsonObject(with: data) as? [String: Any]
            )
            #expect(object.count == 29, "\(kind): expected 29 color tokens, found \(object.count)")
            for (key, value) in object {
                if let string = value as? String {
                    #expect(string.hasPrefix("#"), "\(kind).\(key) is not a hex color")
                } else {
                    #expect(value is [Any], "\(kind).\(key) is neither a color nor an array")
                }
            }
        }
    }

    @Test("the two themes share no surface or text values")
    func themesDoNotShareValues() {
        #expect(Theme.light.color.background != Theme.dark.color.background)
        #expect(Theme.light.color.textPrimary != Theme.dark.color.textPrimary)
        #expect(Theme.light.color.surfaceRaised != Theme.dark.color.surfaceRaised)
        #expect(Theme.light.syntax != Theme.dark.syntax)
    }

    @Test("chart series and heatmap accessors wrap and clamp")
    func chartAccessors() {
        let color = Theme.light.color
        #expect(color.series(0) == color.chartSeries[0])
        #expect(color.series(8) == color.chartSeries[0])
        #expect(color.series(-1) == color.chartSeries[7])
        #expect(color.heatmap(-5) == color.heatmapScale[0])
        #expect(color.heatmap(2) == color.heatmapScale[4])
    }
}
