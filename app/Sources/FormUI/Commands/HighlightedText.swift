import FormCore
import FormDesign
import SwiftUI

/// Draws a string with the core's `{start, len}` match ranges painted in.
///
/// The ranges arrive from `searchSessions` / `searchInSession` already computed in UTF-16
/// (spec 01 §4) and are applied verbatim — this view never re-searches the string, which is
/// the whole reason the core hands back ranges instead of markup. The text is assembled by
/// concatenating runs rather than by converting offsets into `AttributedString` indices,
/// because run concatenation cannot silently land on the wrong grapheme.
///
/// The offsets themselves go through `HighlightGeometry`, which is what makes a range that
/// has gone stale against the text — mid-emoji, past the end, reversed — a duller highlight
/// rather than a crash or a hole in the string.
public struct HighlightedText: View {
    @Environment(\.theme) private var theme

    private let text: String
    private let ranges: [HighlightRange]
    private let currentRange: HighlightRange?
    private let style: KeyPath<TypeTokens, TypeStyle>
    private let foreground: KeyPath<ColorTokens, ThemeColor>

    public init(
        _ text: String,
        ranges: [HighlightRange],
        currentRange: HighlightRange? = nil,
        style: KeyPath<TypeTokens, TypeStyle> = \.caption,
        foreground: KeyPath<ColorTokens, ThemeColor> = \.textSecondary
    ) {
        self.text = text
        self.ranges = ranges
        self.currentRange = currentRange
        self.style = style
        self.foreground = foreground
    }

    public var body: some View {
        Text(attributed)
            .typeStyle(theme.typography[keyPath: style])
            .foregroundStyle(theme.color[keyPath: foreground])
    }

    private var attributed: AttributedString {
        let segments = Self.segments(of: text, ranges: ranges)
        guard segments.contains(where: \.isMatch) else { return AttributedString(text) }

        var result = AttributedString()
        for segment in segments {
            var piece = AttributedString(segment.text)
            if segment.isMatch {
                let isCurrent = currentRange.map { segment.covers($0) } ?? false
                piece.backgroundColor =
                    (isCurrent ? theme.color.accent : theme.color.accentMuted).color
                piece.foregroundColor =
                    (isCurrent ? theme.color.textInverted : theme.color.textPrimary).color
            }
            result.append(piece)
        }
        return result
    }

    // MARK: - Segmentation

    struct Segment: Equatable {
        let text: String
        let isMatch: Bool
        let start: Int
        let length: Int

        func covers(_ range: HighlightRange) -> Bool {
            range.start >= start && range.start < start + length
        }
    }

    /// Splits `text` into alternating plain and matched runs.
    ///
    /// Total by construction: the segments always concatenate back to `text`, whatever the
    /// ranges say. `HighlightGeometry` has already clamped, snapped and merged them, so the
    /// only thing left here is the walk.
    static func segments(of text: String, ranges: [HighlightRange]) -> [Segment] {
        let geometry = HighlightGeometry(text)
        let spans = geometry.spans(for: ranges)
        guard !spans.isEmpty else {
            return [Segment(text: text, isMatch: false, start: 0, length: geometry.utf16Count)]
        }

        var segments: [Segment] = []
        var cursor = 0  // a boundary index, never a raw offset
        for span in spans {
            if span.lower > cursor {
                segments.append(
                    geometry.segment(from: cursor, to: span.lower, isMatch: false))
            }
            segments.append(geometry.segment(from: span.lower, to: span.upper, isMatch: true))
            cursor = span.upper
        }
        if cursor < geometry.boundaryCount - 1 {
            segments.append(
                geometry.segment(from: cursor, to: geometry.boundaryCount - 1, isMatch: false))
        }
        return segments
    }
}

private extension HighlightGeometry {
    func segment(from lower: Int, to upper: Int, isMatch: Bool) -> HighlightedText.Segment {
        let span = Span(lower: lower, upper: upper)
        let start = utf16Offset(of: lower)
        return HighlightedText.Segment(
            text: substring(span),
            isMatch: isMatch,
            start: start,
            length: utf16Offset(of: upper) - start)
    }
}

#Preview("HighlightedText") {
    ThemePreview {
        HighlightedText(
            "…Add a health check endpoint…",
            ranges: [HighlightRange(start: 7, len: 6)],
            style: \.body,
            foreground: \.textPrimary)
        HighlightedText(
            "the ring is stuck at ninety percent, ring after ring",
            ranges: [HighlightRange(start: 4, len: 4), HighlightRange(start: 37, len: 4),
                     HighlightRange(start: 48, len: 4)],
            currentRange: HighlightRange(start: 37, len: 4),
            style: \.body,
            foreground: \.textPrimary)
    }
    .frame(width: 520)
}
