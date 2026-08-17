import FormCore
import Foundation

/// Every time series in the document is keyed by a local `YYYY-MM-DD` string. Charts need
/// `Date`s, so the conversion happens once, here, for all of them.
///
/// This is not aggregation — the buckets, their order and their values are exactly what the
/// core sent (spec 12 §5). Rows whose date does not parse are dropped rather than guessed at.
protocol DatedStat {
    var date: String { get }
}

extension DailyBucket: DatedStat {}
extension CachePoint: DatedStat {}
extension CostPoint: DatedStat {}
extension HeatmapCell: DatedStat {}

struct Dated<Value: DatedStat>: Identifiable {
    let date: Date
    let value: Value

    var id: Date { date }
}

extension Array where Element: DatedStat {
    func dated() -> [Dated<Element>] {
        compactMap { row in
            StatsFormat.date(row.date).map { Dated(date: $0, value: row) }
        }
    }
}

extension Array {
    /// The running total of `value` across the array, for cumulative overlays.
    ///
    /// `CostPoint` in the core carries a `cumulative` field for exactly this, but the Swift
    /// mirror of the document does not expose it yet (reported to W7/W3); until it does, the
    /// overlay is a prefix sum over the core's own per-day values rather than a re-derived
    /// number.
    func runningTotal(_ value: (Element) -> Double) -> [Double] {
        var total = 0.0
        return map { element in
            total += value(element)
            return total
        }
    }
}
