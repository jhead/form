import Testing

@testable import FormDesign

/// Spec 08 §2.5 — the core hands Swift scope *names*; this is where they become colors.
struct SyntaxTests {
    @Test(
        "scopes resolve by longest prefix",
        arguments: [
            ("keyword.control.rust", SyntaxScope.keyword),
            ("keyword.operator.arithmetic", .operator),  // longer prefix wins over `keyword`
            ("constant.numeric.integer.decimal", .number),
            ("constant.language.boolean", .constant),
            ("constant.character.escape.rust", .string),
            ("string.quoted.double.swift", .string),
            ("comment.line.double-slash", .comment),
            ("entity.name.function.swift", .function),
            ("entity.name.type.class.swift", .type),
            ("entity.other.attribute-name.html", .attribute),
            ("storage.type.swift", .type),
            ("storage.modifier.swift", .keyword),
            ("variable.parameter.function", .variable),
            ("support.function.builtin", .function),
            ("punctuation.definition.string.begin", .punctuation),
            ("invalid.illegal.rust", .invalid),
            ("source.rust", .plain),
        ]
    )
    func longestPrefixWins(scope: String, expected: SyntaxScope) {
        #expect(SyntaxTokens.scope(forScope: scope) == expected)
    }

    @Test("the most specific component of a scope stack wins")
    func scopeStacks() {
        #expect(
            SyntaxTokens.scope(forScope: "source.rust meta.function.rust entity.name.function.rust")
                == .function
        )
        #expect(SyntaxTokens.scope(forScope: "source.swift string.quoted.double") == .string)
    }

    @Test("a prefix must end on a scope boundary")
    func prefixesRespectBoundaries() {
        // `keywordish` is not `keyword`.
        #expect(SyntaxTokens.scope(forScope: "keywordish.thing") == .plain)
        #expect(SyntaxTokens.scope(forScope: "stringify") == .plain)
        // The bare token name is still a match.
        #expect(SyntaxTokens.scope(forScope: "keyword") == .keyword)
    }

    @Test("unmatched scopes fall back to plain")
    func unmatchedFallsBackToPlain() {
        for kind in ThemeKind.allCases {
            let theme = kind.theme
            #expect(SyntaxTokens.scope(forScope: "") == .plain)
            #expect(SyntaxTokens.scope(forScope: "nonsense.scope.name") == .plain)
            #expect(theme.syntax.color(forScope: "nonsense") == theme.syntax.plain)
        }
    }

    @Test("plain matches the theme's primary text color", arguments: ThemeKind.allCases)
    func plainIsPrimaryText(kind: ThemeKind) {
        #expect(kind.theme.syntax.plain == kind.theme.color.textPrimary)
    }

    @Test("every scope has a distinct color where it should", arguments: ThemeKind.allCases)
    func scopesAreDistinguishable(kind: ThemeKind) {
        let syntax = kind.theme.syntax
        // Keyword, string, number, comment, function and type are the six a reader relies on
        // to parse a block at a glance; they must not collide.
        let loadBearing = [syntax.keyword, syntax.string, syntax.number, syntax.comment, syntax.function, syntax.type]
        #expect(Set(loadBearing).count == loadBearing.count, "\(kind): load-bearing syntax colors collide")
    }
}
