import Foundation

/// The Overview tab's playful footnote (spec 12 §2): *"You've used ~991× more tokens than
/// The Little Prince."*
///
/// The whole joke lives in this file — table, selection rule and sentence — so changing the
/// tone, or the works, is one edit and touches no view.
enum TokenComparison {
    /// A familiar body of text and roughly what it costs in tokens.
    ///
    /// Counts are word counts × ~1.33, which is the usual English rule of thumb. They are
    /// deliberately round: the footnote is a sense of scale, not a measurement.
    struct Work: Sendable {
        let name: String
        let tokens: Int64
    }

    /// Ascending by size. Add or remove freely — the selection rule only assumes ordering.
    static let works: [Work] = [
        Work(name: "The Little Prince", tokens: 22_000),
        Work(name: "Animal Farm", tokens: 40_000),
        Work(name: "The Great Gatsby", tokens: 63_000),
        Work(name: "The Hobbit", tokens: 127_000),
        Work(name: "Moby-Dick", tokens: 275_000),
        Work(name: "War and Peace", tokens: 780_000),
        Work(name: "the complete works of Shakespeare", tokens: 1_180_000),
    ]

    /// The largest work the total still dwarfs, so the multiplier stays quotable and the
    /// comparison stays impressive. Falls back to the smallest work for a light corpus.
    static func sentence(forTokens total: Int64) -> String? {
        guard total > 0, let smallest = works.first else { return nil }

        guard total >= smallest.tokens * 2 else {
            let fraction = Double(total) / Double(smallest.tokens)
            return "That is about \(StatsFormat.percent(fraction)) of \(smallest.name)."
        }

        let work = works.last { total >= $0.tokens * 3 } ?? smallest
        let multiple = Double(total) / Double(work.tokens)
        return "You've used ~\(multipleLabel(multiple))× more tokens than \(work.name)."
    }

    private static func multipleLabel(_ multiple: Double) -> String {
        multiple < 10
            ? String(format: "%.1f", multiple)
            : StatsFormat.grouped(Int64(multiple.rounded()))
    }
}
