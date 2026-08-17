import FormCore
import FormDesign
import SwiftUI

/// Applies the core's `CodeToken` ranges to the code text (spec 11 §2).
///
/// The core emits `syntect` **scope names with UTF-16 ranges** and never a color (PRD §4.4),
/// so this is the whole of the code-color decision: `SyntaxTokens.color(forScope:)` does the
/// longest-prefix match, and the active theme supplies the value. Ranges are UTF-16 because
/// that is what `NSRange` — and therefore `Range(_:in:)` — speaks, which is why emoji and
/// CJK in a code block land where the core said they would with no re-encoding.
enum CodeHighlighting {
    static func attributed(
        code: String, tokens: [CodeToken], metrics: MarkdownMetrics
    ) -> AttributedString {
        let font = metrics.code.font
        let plain = metrics.theme.syntax.plain

        func piece(_ text: String, _ color: ThemeColor) -> AttributedString {
            var run = AttributedString(text)
            run.font = font
            run.foregroundColor = color.color
            return run
        }

        guard !tokens.isEmpty else { return piece(code, plain) }

        var out = AttributedString()
        var cursor = code.startIndex
        // Defensive: the core emits these in order and non-overlapping, but a renderer that
        // trusts that and is wrong drops code on the floor. Sorting and clamping is cheap.
        for token in tokens.sorted(by: { $0.start < $1.start }) {
            guard token.len > 0,
                let range = Range(NSRange(location: token.start, length: token.len), in: code),
                range.lowerBound >= cursor
            else { continue }
            if cursor < range.lowerBound {
                out += piece(String(code[cursor ..< range.lowerBound]), plain)
            }
            out += piece(String(code[range]), metrics.theme.syntax.color(forScope: token.scope))
            cursor = range.upperBound
        }
        if cursor < code.endIndex { out += piece(String(code[cursor...]), plain) }
        return out
    }

    /// Line count for the gutter. A trailing newline does not start a line.
    static func lineCount(of code: String) -> Int {
        guard !code.isEmpty else { return 1 }
        let trimmed = code.hasSuffix("\n") ? String(code.dropLast()) : code
        return trimmed.reduce(1) { $1 == "\n" ? $0 + 1 : $0 }
    }
}
