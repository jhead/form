import SwiftUI

/// The scope buckets a `syntect` scope name can land in. The core returns scope *names*
/// with ranges and never a color (PRD §4.4); this is where the mapping happens.
public enum SyntaxScope: String, Sendable, Codable, CaseIterable {
    case plain, keyword, string, number, comment, function, type
    case variable, constant, `operator`, punctuation, attribute, invalid
}

/// Scope → color. Resolution is longest-prefix-match on the scope string; anything
/// unmatched falls back to `plain` (spec 08 §2.5).
public struct SyntaxTokens: Sendable, Equatable, Codable {
    public var plain: ThemeColor
    public var keyword: ThemeColor
    public var string: ThemeColor
    public var number: ThemeColor
    public var comment: ThemeColor
    public var function: ThemeColor
    public var type: ThemeColor
    public var variable: ThemeColor
    public var constant: ThemeColor
    public var `operator`: ThemeColor
    public var punctuation: ThemeColor
    public var attribute: ThemeColor
    public var invalid: ThemeColor

    public init(
        plain: ThemeColor,
        keyword: ThemeColor,
        string: ThemeColor,
        number: ThemeColor,
        comment: ThemeColor,
        function: ThemeColor,
        type: ThemeColor,
        variable: ThemeColor,
        constant: ThemeColor,
        operator: ThemeColor,
        punctuation: ThemeColor,
        attribute: ThemeColor,
        invalid: ThemeColor
    ) {
        self.plain = plain
        self.keyword = keyword
        self.string = string
        self.number = number
        self.comment = comment
        self.function = function
        self.type = type
        self.variable = variable
        self.constant = constant
        self.operator = `operator`
        self.punctuation = punctuation
        self.attribute = attribute
        self.invalid = invalid
    }

    public func color(for scope: SyntaxScope) -> ThemeColor {
        switch scope {
        case .plain: plain
        case .keyword: keyword
        case .string: string
        case .number: number
        case .comment: comment
        case .function: function
        case .type: type
        case .variable: variable
        case .constant: constant
        case .operator: self.operator
        case .punctuation: punctuation
        case .attribute: attribute
        case .invalid: invalid
        }
    }

    /// `scope` is a TextMate/`syntect` scope string, possibly a space-separated stack such
    /// as `"source.rust meta.function.rust entity.name.function.rust"`. The most specific
    /// component wins, then the longest matching prefix within it.
    public func color(forScope scope: String) -> ThemeColor {
        color(for: Self.scope(forScope: scope))
    }

    public static func scope(forScope scope: String) -> SyntaxScope {
        let components = scope.split(separator: " ").map(String.init)
        for component in components.reversed() {
            if let match = longestPrefixMatch(component) { return match }
        }
        return .plain
    }

    private static func longestPrefixMatch(_ component: String) -> SyntaxScope? {
        var best: (length: Int, scope: SyntaxScope)?
        for (prefix, scope) in prefixTable where component.hasPrefix(prefix) {
            // The prefix must end at a scope boundary: `keyword` matches `keyword.control`
            // but must not match `keywordish`.
            let next = component.index(component.startIndex, offsetBy: prefix.count)
            if next != component.endIndex, component[next] != "." { continue }
            if best == nil || prefix.count > best!.length {
                best = (prefix.count, scope)
            }
        }
        return best?.scope
    }

    /// Ordered longest-first is unnecessary — `longestPrefixMatch` scans all of it — but the
    /// grouping documents intent. Derived from the TextMate scope naming conventions that
    /// `syntect`'s bundled syntaxes follow.
    static let prefixTable: [(String, SyntaxScope)] = [
        ("comment", .comment),
        ("string", .string),
        ("constant.numeric", .number),
        ("constant.character.escape", .string),
        ("constant", .constant),
        ("keyword.operator", .operator),
        ("keyword", .keyword),
        ("storage.type", .type),
        ("storage.modifier", .keyword),
        ("storage", .keyword),
        ("entity.name.function", .function),
        ("entity.name.type", .type),
        ("entity.name.class", .type),
        ("entity.name.struct", .type),
        ("entity.name.enum", .type),
        ("entity.name.namespace", .type),
        ("entity.name.tag", .keyword),
        ("entity.other.attribute-name", .attribute),
        ("entity.other.inherited-class", .type),
        ("entity.name", .function),
        ("support.function", .function),
        ("support.class", .type),
        ("support.type", .type),
        ("support.constant", .constant),
        ("support.variable", .variable),
        ("variable.function", .function),
        ("variable.parameter", .variable),
        ("variable", .variable),
        ("meta.attribute", .attribute),
        ("meta.annotation", .attribute),
        ("punctuation", .punctuation),
        ("invalid", .invalid),
        ("markup.deleted", .invalid),
        ("source", .plain),
        ("text", .plain),
    ]
}
