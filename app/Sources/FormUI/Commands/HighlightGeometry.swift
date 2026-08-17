import FormCore
import Foundation

/// Character-boundary geometry for applying the core's UTF-16 highlight ranges to a string.
///
/// The core hands back `{start, len}` in UTF-16 code units (spec 01 §4) and they are applied
/// verbatim rather than re-derived. "Verbatim" still has to survive two things a naive slice
/// does not:
///
/// 1. **A range that is not on a `Character` boundary.** An emoji or a CJK character occupies
///    several UTF-16 units, and a range can land inside one — a stale range from a transcript
///    that has moved under a streaming update is the obvious way. Converting such an offset
///    to a `String.Index` fails, and a slice built from it silently drops the run: the reader
///    sees text with characters missing. Here, a lower bound rounds **down** and an upper
///    bound rounds **up**, so a partial hit highlights the whole grapheme and the string is
///    always reproduced in full.
///
/// 2. **A range that is out of step with the string entirely.** `HighlightRange.range(in:)`
///    is documented to return `nil` when the range does not fall inside the string, and it
///    does for offsets past the end — but it **traps** on a negative `start` or `len`:
///    `index(_:offsetBy:limitedBy:)` does not limit against a bound that is behind the
///    direction of travel, so the walk either runs off the front ("String index is out of
///    bounds") or produces an upper bound below the lower one ("Range requires
///    lowerBound <= upperBound"). Nothing in `Commands/` calls it; every offset is clamped
///    into `0...utf16Count` here first. See the W14 report.
struct HighlightGeometry {
    /// A span of the string, as indices into the boundary table — never raw offsets, so a
    /// slice cannot be built from a position that is not a real `Character` boundary.
    struct Span: Equatable {
        let lower: Int
        let upper: Int
    }

    enum Rounding { case down, up }

    let text: String
    /// UTF-16 offset of every `Character` boundary, `0` first and `utf16Count` last.
    private let offsets: [Int]
    /// The `String.Index` at each of those boundaries.
    private let indices: [String.Index]

    init(_ text: String) {
        self.text = text
        var offsets = [0]
        var indices = [text.startIndex]
        var offset = 0
        var index = text.startIndex
        while index < text.endIndex {
            offset += String(text[index]).utf16.count
            index = text.index(after: index)
            offsets.append(offset)
            indices.append(index)
        }
        self.offsets = offsets
        self.indices = indices
    }

    var utf16Count: Int { offsets[offsets.count - 1] }
    var boundaryCount: Int { offsets.count }
    var wholeSpan: Span { Span(lower: 0, upper: offsets.count - 1) }

    func utf16Offset(of boundary: Int) -> Int {
        offsets[min(max(boundary, 0), offsets.count - 1)]
    }

    func substring(_ span: Span) -> String {
        String(text[indices[span.lower]..<indices[span.upper]])
    }

    /// Clamped, snapped, sorted and merged. Degenerate input — negative, reversed,
    /// overlapping, past the end — reduces to fewer spans rather than to a crash.
    func spans(for ranges: [HighlightRange]) -> [Span] {
        var spans: [Span] = []
        for range in ranges {
            let start = min(max(range.start, 0), utf16Count)
            let end = min(start + max(range.len, 0), utf16Count)
            guard end > start else { continue }
            let lower = boundary(forUTF16: start, rounding: .down)
            let upper = boundary(forUTF16: end, rounding: .up)
            guard upper > lower else { continue }
            spans.append(Span(lower: lower, upper: upper))
        }
        spans.sort { $0.lower < $1.lower }

        var merged: [Span] = []
        for span in spans {
            if let last = merged.last, span.lower <= last.upper {
                merged[merged.count - 1] = Span(
                    lower: last.lower, upper: max(last.upper, span.upper))
            } else {
                merged.append(span)
            }
        }
        return merged
    }

    /// The boundary at, or on the requested side of, a UTF-16 offset.
    func boundary(forUTF16 offset: Int, rounding: Rounding) -> Int {
        let clamped = min(max(offset, 0), utf16Count)
        var low = 0
        var high = offsets.count - 1
        while low < high {
            let mid = (low + high) / 2
            if offsets[mid] < clamped { low = mid + 1 } else { high = mid }
        }
        if offsets[low] == clamped { return low }
        return rounding == .down ? max(0, low - 1) : low
    }

    // MARK: - Word boundaries

    func character(before boundary: Int) -> Character? {
        guard boundary > 0, boundary < indices.count else { return nil }
        return text[indices[boundary - 1]]
    }

    func character(at boundary: Int) -> Character? {
        guard boundary >= 0, boundary < indices.count - 1 else { return nil }
        return text[indices[boundary]]
    }

    /// Whether a span is flanked by non-word characters — the `⌘F` whole-word toggle.
    func isWholeWord(_ span: Span) -> Bool {
        let isWordCharacter: (Character) -> Bool = { $0.isLetter || $0.isNumber || $0 == "_" }
        if let before = character(before: span.lower), isWordCharacter(before) { return false }
        if let after = character(at: span.upper), isWordCharacter(after) { return false }
        return true
    }
}
