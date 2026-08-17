import Testing
@testable import FormDesign

/// Named cases keep failure output to `light` / `dark` instead of dumping a whole `Theme`.
enum ThemeKind: String, CaseIterable, Sendable, CustomStringConvertible {
    case light, dark
    var theme: Theme { self == .light ? .light : .dark }
    var description: String { rawValue }
}

/// WCAG AA over the full token matrix, both themes (spec 08 §2.1).
///
/// **The bar, and why it is tiered.** WCAG 2.1 SC 1.4.3 asks 4.5:1 of body text and 3:1 of
/// text at ≥18 pt (or ≥14 pt bold); SC 1.4.11 asks 3:1 of non-text UI components and
/// graphical objects. This suite applies that split literally rather than demanding 4.5:1 of
/// everything:
///
/// - `textPrimary` / `textSecondary` carry running prose → **4.5:1** on every surface.
/// - `textTertiary`, the semantic colors, the chart series and the indicator colors are
///   de-emphasised labels, glyphs, rules and marks → **3:1** on every surface.
/// - Borders and grid lines are structural, not informational → a bare visibility floor.
///   That check is a "somebody set the hairline to the background color" guard, not a WCAG
///   criterion; the dark anchor #33302C on #1A1917 measures 1.18:1 by design.
///
/// Every pair that falls below its tier must be listed in `knownExceptions` with a reason,
/// and a second test fails if an entry there has become unnecessary. That way the matrix
/// cannot quietly regress and the exceptions cannot quietly accumulate.
struct ContrastTests {
    static let bodyMinimum = 4.5
    static let largeMinimum = 3.0
    static let structuralMinimum = 1.15

    /// Pairs the specified anchors cannot satisfy. Keep this list at zero-or-justified.
    static let knownExceptions: [Exception] = [
        Exception(
            theme: "light",
            foreground: "textInverted",
            background: "accent",
            floor: 4.0,
            reason: """
            White on the spec's light accent anchor #C15F3C measures 4.23:1 — the primary \
            button's resting fill. Fixing it means moving the brand color to about #BA5636, \
            which is a product decision, not a design-system one. Floored at 4.0 so it \
            cannot drift further.
            """
        ),
    ]

    struct Exception: Sendable {
        let theme: String
        let foreground: String
        let background: String
        let floor: Double
        let reason: String
    }

    // MARK: Body text

    @Test("body text clears WCAG AA on every surface", arguments: ThemeKind.allCases)
    func bodyTextContrast(kind: ThemeKind) {
        let theme = kind.theme
        for (fgName, fg) in theme.color.bodyTextTokens {
            for (bgName, bg) in theme.color.surfaces {
                assertContrast(
                    theme: theme, fg: fg, fgName: fgName, bg: bg, bgName: bgName,
                    tier: Self.bodyMinimum
                )
            }
        }
    }

    // MARK: De-emphasised text, indicators and chart marks

    @Test("accented tokens clear the 3:1 large-text / non-text bar", arguments: ThemeKind.allCases)
    func accentedContrast(kind: ThemeKind) {
        let theme = kind.theme
        for (fgName, fg) in theme.color.accentedTokens {
            for (bgName, bg) in theme.color.surfaces {
                assertContrast(
                    theme: theme, fg: fg, fgName: fgName, bg: bg, bgName: bgName,
                    tier: Self.largeMinimum
                )
            }
        }
    }

    // MARK: Inverted text on filled surfaces

    @Test("textInverted is readable on every fill it lands on", arguments: ThemeKind.allCases)
    func invertedTextContrast(kind: ThemeKind) {
        let theme = kind.theme
        for (bgName, bg) in theme.color.invertedTextBackings {
            assertContrast(
                theme: theme,
                fg: theme.color.textInverted, fgName: "textInverted",
                bg: bg, bgName: bgName,
                tier: Self.bodyMinimum
            )
        }
    }

    // MARK: Syntax

    @Test("syntax colors are legible in a code panel", arguments: ThemeKind.allCases)
    func syntaxContrast(kind: ThemeKind) {
        let theme = kind.theme
        // Comments and punctuation are deliberately recessive; the rest is body-weight code.
        let recessive: Set<SyntaxScope> = [.comment, .punctuation]
        for scope in SyntaxScope.allCases {
            let tier = recessive.contains(scope) ? Self.largeMinimum : Self.bodyMinimum
            for (bgName, bg) in [
                ("surfaceRaised", theme.color.surfaceRaised),
                ("background", theme.color.background),
            ] {
                assertContrast(
                    theme: theme,
                    fg: theme.syntax.color(for: scope), fgName: "syntax.\(scope.rawValue)",
                    bg: bg, bgName: bgName,
                    tier: tier
                )
            }
        }
    }

    // MARK: Structure

    @Test("borders and grid lines are visible without being loud", arguments: ThemeKind.allCases)
    func borderVisibility(kind: ThemeKind) {
        let theme = kind.theme
        // Hairlines are drawn on `background` and `surface` — spec 08 §1 puts panel borders
        // there, never around a `surfaceRaised` bubble — so those are the pairs that matter.
        let pairs: [(String, ThemeColor, String, ThemeColor)] = [
            ("border", theme.color.border, "background", theme.color.background),
            ("border", theme.color.border, "surface", theme.color.surface),
            ("borderStrong", theme.color.borderStrong, "background", theme.color.background),
            ("chartGrid", theme.color.chartGrid, "background", theme.color.background),
        ]
        for (fgName, fg, bgName, bg) in pairs {
            let ratio = fg.contrastRatio(against: bg)
            #expect(
                ratio >= Self.structuralMinimum,
                "\(theme.id): \(fgName) on \(bgName) is \(rounded(ratio)):1, below the \(Self.structuralMinimum):1 visibility floor"
            )
        }
        #expect(theme.color.borderStrong.contrastRatio(against: theme.color.background)
            > theme.color.border.contrastRatio(against: theme.color.background),
            "\(theme.id): borderStrong must be stronger than border")
    }

    // MARK: Scales

    @Test("hover and selected surfaces are distinguishable from their base", arguments: ThemeKind.allCases)
    func surfaceStatesAreDistinct(kind: ThemeKind) {
        let theme = kind.theme
        let base = theme.color.backgroundSidebar
        for (name, surface) in [
            ("surfaceHover", theme.color.surfaceHover),
            ("surfaceSelected", theme.color.surfaceSelected),
        ] {
            let ratio = surface.contrastRatio(against: base)
            #expect(ratio >= 1.05, "\(theme.id): \(name) is invisible against backgroundSidebar (\(rounded(ratio)):1)")
        }
        #expect(theme.color.surfaceSelected != theme.color.surfaceHover)
    }

    @Test("the heatmap scale is monotonic and its top stop is legible", arguments: ThemeKind.allCases)
    func heatmapScale(kind: ThemeKind) {
        let theme = kind.theme
        let stops = theme.color.heatmapScale
        #expect(stops.count == 5, "\(theme.id): heatmapScale must have 5 stops")
        let luminances = stops.map(\.relativeLuminance)
        let ascending = zip(luminances, luminances.dropFirst()).allSatisfy { $0 < $1 }
        let descending = zip(luminances, luminances.dropFirst()).allSatisfy { $0 > $1 }
        #expect(ascending || descending, "\(theme.id): heatmapScale luminance is not monotonic: \(luminances)")

        if let top = stops.last {
            let ratio = top.contrastRatio(against: theme.color.background)
            #expect(ratio >= Self.largeMinimum, "\(theme.id): the most intense heatmap stop is \(rounded(ratio)):1 on background")
        }
    }

    @Test("chart series are eight distinct colors", arguments: ThemeKind.allCases)
    func chartSeriesAreDistinct(kind: ThemeKind) {
        let theme = kind.theme
        let series = theme.color.chartSeries
        #expect(series.count == 8, "\(theme.id): expected 8 chart series, got \(series.count)")
        #expect(Set(series).count == series.count, "\(theme.id): chart series contains duplicates")
    }

    // MARK: Exception hygiene

    @Test("every documented exception is still necessary")
    func exceptionsAreMinimal() throws {
        for exception in Self.knownExceptions {
            let theme = try #require(Theme.all.first { $0.id == exception.theme })
            let fg = try #require(namedColor(exception.foreground, in: theme))
            let bg = try #require(namedColor(exception.background, in: theme))
            let ratio = fg.contrastRatio(against: bg)
            #expect(
                ratio < Self.bodyMinimum,
                """
                \(exception.theme): \(exception.foreground) on \(exception.background) now \
                measures \(rounded(ratio)):1 and no longer needs an exception — delete it.
                """
            )
        }
    }

    // MARK: Helpers

    private func assertContrast(
        theme: Theme, fg: ThemeColor, fgName: String, bg: ThemeColor, bgName: String, tier: Double
    ) {
        let ratio = fg.contrastRatio(against: bg)
        let exception = Self.knownExceptions.first {
            $0.theme == theme.id && $0.foreground == fgName && $0.background == bgName
        }
        let floor = exception?.floor ?? tier
        #expect(
            ratio >= floor,
            """
            \(theme.id): \(fgName) on \(bgName) is \(rounded(ratio)):1, \
            below the required \(floor):1\(exception == nil ? "" : " (documented exception)")
            """
        )
    }

    private func namedColor(_ name: String, in theme: Theme) -> ThemeColor? {
        let table: [String: ThemeColor] = [
            "background": theme.color.background,
            "backgroundSidebar": theme.color.backgroundSidebar,
            "surface": theme.color.surface,
            "surfaceRaised": theme.color.surfaceRaised,
            "surfaceSelected": theme.color.surfaceSelected,
            "surfaceHover": theme.color.surfaceHover,
            "textPrimary": theme.color.textPrimary,
            "textSecondary": theme.color.textSecondary,
            "textTertiary": theme.color.textTertiary,
            "textInverted": theme.color.textInverted,
            "accent": theme.color.accent,
            "accentHover": theme.color.accentHover,
            "success": theme.color.success,
            "warning": theme.color.warning,
            "danger": theme.color.danger,
            "info": theme.color.info,
        ]
        return table[name]
    }

    private func rounded(_ value: Double) -> String {
        String(format: "%.2f", value)
    }
}
