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

    /// Splits `text` into alternating plain and matched runs. Ranges are clamped, sorted and
    /// merged first, so overlapping or out-of-bounds input from a stale index degrades into
    /// plain text instead of crashing.
    static func segments(of text: String, ranges: [HighlightRange]) -> [Segment] {
        let total = text.utf16.count
        let merged = merge(ranges, limit: total)
        guard !merged.isEmpty else { return [Segment(text: text, isMatch: false, start: 0, length: total)] }

        var segments: [Segment] = []
        var cursor = 0
        for range in merged {
            if range.start > cursor, let plain = substring(text, from: cursor, to: range.start) {
                segments.append(
                    Segment(text: plain, isMatch: false, start: cursor, length: range.start - cursor))
            }
            if let match = substring(text, from: range.start, to: range.start + range.len) {
                segments.append(
                    Segment(text: match, isMatch: true, start: range.start, length: range.len))
            }
            cursor = range.start + range.len
        }
        if cursor < total, let tail = substring(text, from: cursor, to: total) {
            segments.append(Segment(text: tail, isMatch: false, start: cursor, length: total - cursor))
        }
        return segments
    }

    static func merge(_ ranges: [HighlightRange], limit: Int) -> [HighlightRange] {
        let clamped = ranges
            .map { HighlightRange(start: max(0, min($0.start, limit)), len: max(0, $0.len)) }
            .map { HighlightRange(start: $0.start, len: min($0.len, limit - $0.start)) }
            .filter { $0.len > 0 }
            .sorted { $0.start < $1.start }

        var merged: [HighlightRange] = []
        for range in clamped {
            if let last = merged.last, range.start <= last.start + last.len {
                let end = max(last.start + last.len, range.start + range.len)
                merged[merged.count - 1] = HighlightRange(start: last.start, len: end - last.start)
            } else {
                merged.append(range)
            }
        }
        return merged
    }

    private static func substring(_ text: String, from: Int, to: Int) -> String? {
        HighlightRange(start: from, len: to - from).range(in: text).map { String(text[$0]) }
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
