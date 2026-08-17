import Foundation
import Testing

@testable import FormDesign

struct TypographyTests {
    @Test("the scale matches spec 08 §2.2")
    func scaleMatchesSpec() {
        let base = TypeTokens.Scale.standard
        let expected: [(String, CGFloat, FontWeightToken, FontFamily)] = [
            ("wordmark", 20, .regular, .serif),
            ("display", 28, .regular, .serif),
            ("title", 17, .semibold, .sans),
            ("heading", 15, .semibold, .sans),
            ("body", 14, .regular, .sans),
            ("bodyStrong", 14, .semibold, .sans),
            ("ui", 13, .regular, .sans),
            ("uiMedium", 13, .medium, .sans),
            ("caption", 12, .regular, .sans),
            ("micro", 11, .regular, .sans),
            ("code", 12.5, .regular, .mono),
            ("codeInline", 13, .regular, .mono),
        ]
        let actual = Dictionary(uniqueKeysWithValues: base.all.map { ($0.name, $0.style) })
        for (name, size, weight, family) in expected {
            let style = actual[name]
            #expect(style?.size == size, "\(name) size")
            #expect(style?.weight == weight, "\(name) weight")
            #expect(style?.family == family, "\(name) family")
        }
        #expect(actual["body"]?.lineHeight == 1.55)
        #expect(actual["code"]?.lineHeight == 1.5)
    }

    /// Serif is reserved for the wordmark and display headings (PRD §1). Nothing else.
    @Test("serif is used only by the wordmark and display")
    func serifIsReserved() {
        let serifStyles = TypeTokens.Scale.standard.all
            .filter { $0.style.family == .serif }
            .map(\.name)
        #expect(Set(serifStyles) == ["wordmark", "display"])
    }

    @Test("mono is used only by code styles")
    func monoIsForCode() {
        let monoStyles = TypeTokens.Scale.standard.all
            .filter { $0.style.family == .mono }
            .map(\.name)
        #expect(Set(monoStyles) == ["code", "codeInline"])
    }

    @Test("the text-size multiplier scales every style and clamps to 0.85…1.4")
    func textScaleAppliesAndClamps() {
        let typography = TypeTokens()
        #expect(typography.body.size == 14)

        let large = typography.withScale(1.4)
        #expect(large.scale == 1.4)
        for (name, style) in TypeTokens.Scale.standard.all {
            let scaled = scaledStyle(named: name, in: large)
            #expect(scaled?.size == style.size * 1.4, "\(name) did not scale")
        }

        #expect(typography.withScale(3.0).scale == TypeTokens.maximumScale)
        #expect(typography.withScale(0.1).scale == TypeTokens.minimumScale)
        #expect(typography.withScale(3.0).body.size == 14 * 1.4)
    }

    @Test("a smuggled scale in theme JSON is clamped on decode")
    func decodedScaleIsClamped() throws {
        let json = Data(#"{"scale": 9.0}"#.utf8)
        let decoded = try JSONDecoder().decode(TypeTokens.self, from: json)
        #expect(decoded.scale == TypeTokens.maximumScale)
        #expect(decoded.base == .standard)
    }

    @Test("line height converts to the leading SwiftUI wants")
    func lineSpacing() {
        let body = TypeTokens().body
        #expect(abs(body.lineSpacing - (14 * 1.55 - 14)) < 0.0001)
        #expect(TypeTokens().ui.lineSpacing == 0)
    }

    @Test("every family resolves to a usable font")
    func familiesResolve() {
        for family in FontFamily.allCases {
            let font = FontResolver.nsFont(family: family, size: 13, weight: .regular)
            #expect(font.pointSize == 13, "\(family) lost its size")
        }
        // Serif must not silently fall back to the sans system face.
        let serif = FontResolver.nsFont(family: .serif, size: 20, weight: .regular)
        let sans = FontResolver.nsFont(family: .sans, size: 20, weight: .regular)
        #expect(serif.fontName != sans.fontName, "serif resolved to the system sans face")

        let mono = FontResolver.nsFont(family: .mono, size: 13, weight: .regular)
        #expect(mono.fontName != sans.fontName, "mono resolved to the system sans face")
    }

    private func scaledStyle(named name: String, in tokens: TypeTokens) -> TypeStyle? {
        switch name {
        case "wordmark": tokens.wordmark
        case "display": tokens.display
        case "title": tokens.title
        case "heading": tokens.heading
        case "body": tokens.body
        case "bodyStrong": tokens.bodyStrong
        case "ui": tokens.ui
        case "uiMedium": tokens.uiMedium
        case "caption": tokens.caption
        case "micro": tokens.micro
        case "code": tokens.code
        case "codeInline": tokens.codeInline
        default: nil
        }
    }
}
