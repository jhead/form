import Foundation

/// Every number the dashboard prints (spec 12 §3).
///
/// One place, because an axis label and the tooltip that explains it must never disagree
/// about what `1.2M` stood for. Nothing here aggregates — it only renders values the
/// `UsageStats` document already contains.
enum StatsFormat {
    // MARK: Counts

    /// `947` · `23.6k` · `21.8M` — the axis and tile form.
    static func abbreviated(_ value: Int64) -> String {
        abbreviated(Double(value))
    }

    static func abbreviated(_ value: Double) -> String {
        let magnitude = abs(value)
        switch magnitude {
        case 0 ..< 1_000:
            return String(Int64(value.rounded()))
        case 1_000 ..< 1_000_000:
            return scaled(value, by: 1_000, suffix: "k")
        case 1_000_000 ..< 1_000_000_000:
            return scaled(value, by: 1_000_000, suffix: "M")
        default:
            return scaled(value, by: 1_000_000_000, suffix: "B")
        }
    }

    /// `23,627` — the full value, for the tooltip behind an abbreviation.
    static func grouped(_ value: Int64) -> String {
        value.formatted(.number.grouping(.automatic))
    }

    static func grouped(_ value: Double) -> String {
        value.formatted(.number.precision(.fractionLength(0)).grouping(.automatic))
    }

    /// One decimal, dropped when it is a zero: `1.2k`, not `1.0k`.
    private static func scaled(_ value: Double, by divisor: Double, suffix: String) -> String {
        let scaled = value / divisor
        let rounded = (scaled * 10).rounded() / 10
        if rounded == rounded.rounded() {
            return "\(Int64(rounded))\(suffix)"
        }
        return String(format: "%.1f%@", rounded, suffix)
    }

    // MARK: Money

    /// `$0.00` at the usual scale; `$1.2k` once an axis would otherwise wrap, and enough
    /// decimals to stop a real-but-tiny daily spend rendering as `$0.00`.
    static func currency(_ value: Double) -> String {
        let magnitude = abs(value)
        if magnitude >= 1_000 { return "$" + scaled(value, by: 1_000, suffix: "k") }
        if magnitude > 0, magnitude < 0.01 { return String(format: "$%.4f", value) }
        return String(format: "$%.2f", value)
    }

    /// Always two decimals — headline figures, where a shifting decimal count reads as noise.
    static func currencyExact(_ value: Double) -> String {
        String(format: "$%.2f", value)
    }

    // MARK: Time

    /// `340ms` · `1.2s` · `3m 31s` · `1h 12m`.
    static func duration(ms: Int64) -> String {
        duration(ms: Double(ms))
    }

    static func duration(ms: Double) -> String {
        guard ms > 0 else { return "0ms" }
        if ms < 1_000 { return "\(Int64(ms.rounded()))ms" }
        let seconds = ms / 1_000
        if seconds < 60 { return String(format: "%.1fs", seconds) }
        let total = Int64(seconds.rounded())
        if total < 3_600 { return "\(total / 60)m \(total % 60)s" }
        return "\(total / 3_600)h \((total % 3_600) / 60)m"
    }

    /// `15:00` in the user's clock convention — the hour histogram's axis.
    static func hour(_ hour: Int) -> String {
        let clamped = max(0, min(23, hour))
        var components = DateComponents()
        components.hour = clamped
        components.minute = 0
        guard let date = Calendar.current.date(from: components) else { return "\(clamped)" }
        return date.formatted(.dateTime.hour())
    }

    /// The compact axis form: only every third hour carries a label.
    static func hourShort(_ hour: Int) -> String {
        let clamped = max(0, min(23, hour))
        return "\(clamped)"
    }

    static let weekdayNames = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]

    // MARK: Rates and ratios

    /// `62%` — `fraction` is 0…1.
    static func percent(_ fraction: Double, decimals: Int = 0) -> String {
        String(format: "%.\(decimals)f%%", fraction * 100)
    }

    /// `48 tok/s`.
    static func rate(_ tokensPerSecond: Double) -> String {
        String(format: "%.0f tok/s", tokensPerSecond)
    }

    // MARK: Dates

    /// `YYYY-MM-DD` in the caller's timezone — the form every bucket in the document uses.
    /// Parsed by hand rather than through a `DateFormatter` because the strings are already
    /// local dates and a formatter would only add a timezone to get wrong.
    static func date(_ ymd: String) -> Date? {
        let parts = ymd.split(separator: "-")
        guard parts.count == 3,
            let year = Int(parts[0]), let month = Int(parts[1]), let day = Int(parts[2])
        else { return nil }
        var components = DateComponents()
        components.year = year
        components.month = month
        components.day = day
        components.hour = 12  // midday, so a DST shift cannot move the bucket to another day
        return Calendar.current.date(from: components)
    }

    /// `Mar 4` — axis ticks and tooltip titles.
    static func shortDate(_ date: Date) -> String {
        date.formatted(.dateTime.month(.abbreviated).day())
    }

    /// `Tue, Mar 4` — the heatmap's hover detail.
    static func longDate(_ date: Date) -> String {
        date.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day())
    }

    static func monthName(_ date: Date) -> String {
        date.formatted(.dateTime.month(.abbreviated))
    }
}

/// A per-model display name that fits a chart legend. The document carries `displayName`
/// for models; providers and leaderboard rows only carry ids, so this is the fallback.
extension String {
    var titleCasedIdentifier: String {
        split(whereSeparator: { $0 == "-" || $0 == "_" })
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
    }
}
