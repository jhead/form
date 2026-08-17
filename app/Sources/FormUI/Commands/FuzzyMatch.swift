import FormCore
import Foundation

/// Subsequence matching for the palette's **Commands** and **Groups** sections.
///
/// Session hits come from the core's FTS index and arrive with their ranges already
/// computed; this is only for the two sections that match against in-memory strings the core
/// has never seen. It emits the same `{start, len}` UTF-16 shape so one highlighting view
/// draws both.
enum FuzzyMatch {
    /// `nil` when the query is not a subsequence of the title or any keyword.
    static func score(_ query: String, in title: String, keywords: [String] = []) -> Double? {
        var best: Double?
        if let direct = subsequence(query, in: title) {
            best = direct.score
        }
        for keyword in keywords {
            guard let hit = subsequence(query, in: keyword) else { continue }
            // A keyword match is real but weaker than matching the visible title.
            let discounted = hit.score * 0.6
            if discounted > (best ?? -.infinity) { best = discounted }
        }
        return best
    }

    /// Ranges to highlight in `text`, or `nil` when it does not match at all.
    static func ranges(of query: String, in text: String) -> [HighlightRange]? {
        subsequence(query, in: text)?.ranges
    }

    private struct Match {
        let score: Double
        let ranges: [HighlightRange]
    }

    /// Greedy left-to-right subsequence match, scored for contiguity and word boundaries —
    /// so "ns" finds "New Session" ahead of "Transcript Snapshot".
    private static func subsequence(_ query: String, in text: String) -> Match? {
        let needle = Array(query.lowercased().filter { !$0.isWhitespace })
        guard !needle.isEmpty else { return Match(score: 0, ranges: []) }

        let characters = Array(text)
        let lowered = Array(text.lowercased())
        // `lowercased()` can change length (ß → ss); fall back to a plain contains check
        // rather than emit ranges that would not line up with the original string.
        guard lowered.count == characters.count else {
            return text.localizedCaseInsensitiveContains(query) ? Match(score: 0.1, ranges: []) : nil
        }

        // UTF-16 offset of each character, so ranges are in the unit HighlightRange uses.
        var offsets = [Int](repeating: 0, count: characters.count + 1)
        for (index, character) in characters.enumerated() {
            offsets[index + 1] = offsets[index] + String(character).utf16.count
        }

        var matched: [Int] = []
        var cursor = 0
        for wanted in needle {
            var found = false
            while cursor < lowered.count {
                if lowered[cursor] == wanted {
                    matched.append(cursor)
                    cursor += 1
                    found = true
                    break
                }
                cursor += 1
            }
            guard found else { return nil }
        }

        var score = 1.0
        var runs: [HighlightRange] = []
        var runStart = matched[0]
        var previous = matched[0]

        for index in matched.dropFirst() {
            if index == previous + 1 {
                score += 0.35  // contiguous characters are what a user expects to see
            } else {
                runs.append(range(from: runStart, through: previous, offsets: offsets))
                runStart = index
            }
            previous = index
        }
        runs.append(range(from: runStart, through: previous, offsets: offsets))

        if matched[0] == 0 { score += 1.0 }
        if isWordStart(matched[0], in: characters) { score += 0.5 }
        // Prefer the tighter target when two titles match equally well.
        score += 1.0 / Double(characters.count + 1)
        if characters.count == needle.count { score += 0.5 }

        return Match(score: score, ranges: runs)
    }

    private static func range(from start: Int, through end: Int, offsets: [Int]) -> HighlightRange {
        HighlightRange(start: offsets[start], len: offsets[end + 1] - offsets[start])
    }

    private static func isWordStart(_ index: Int, in characters: [Character]) -> Bool {
        guard index > 0 else { return true }
        let previous = characters[index - 1]
        return !(previous.isLetter || previous.isNumber)
    }
}
