import Foundation
import FormCore

/// The strings the transcript and composer print: `3m 31s`, `5.9k`, `+268`, `2m ago`.
///
/// Pure and `static` so the reference's exact wording (spec 08 §1) is assertable without
/// standing up a view.
public enum ChatFormat {
    /// `1,204` — grouped, for popover rows where the exact number is the point.
    public static func exact(_ count: Int64) -> String {
        Self.grouping.string(from: NSNumber(value: count)) ?? String(count)
    }

    /// `5.9k` — the turn footer's form. One significant decimal below 10k, none above.
    public static func compact(_ count: Int64) -> String {
        let magnitude = abs(count)
        let sign = count < 0 ? "-" : ""
        switch magnitude {
        case ..<1_000:
            return "\(count)"
        case ..<10_000:
            return sign + trimmed(Double(magnitude) / 1_000, places: 1) + "k"
        case ..<1_000_000:
            return sign + "\((magnitude + 500) / 1_000)k"
        default:
            return sign + trimmed(Double(magnitude) / 1_000_000, places: 1) + "M"
        }
    }

    /// `840ms` · `1.4s` · `3m 31s` · `1h 04m`. Elapsed wall time, never a clock time.
    public static func duration(_ milliseconds: Int64) -> String {
        let ms = max(0, milliseconds)
        if ms < 1_000 { return "\(ms)ms" }
        let seconds = Double(ms) / 1_000
        if seconds < 60 { return trimmed(seconds, places: 1) + "s" }
        let whole = Int64(seconds.rounded())
        let minutes = whole / 60
        if minutes < 60 { return "\(minutes)m \(whole % 60)s" }
        return String(format: "%dh %02dm", minutes / 60, minutes % 60)
    }

    /// `$0.42`, `$1.2k` past four figures. Sub-cent amounts keep enough digits to be honest.
    public static func cost(_ value: Double) -> String {
        if value == 0 { return "$0.00" }
        if value < 0.01 { return "$" + trimmed(value, places: 4) }
        if value < 1_000 { return "$" + String(format: "%.2f", value) }
        return "$" + compact(Int64(value.rounded()))
    }

    /// `just now` · `12m ago` · `3h ago` · `yesterday` · `14 Aug`. Shown beside the hover
    /// actions on a user message (F1.5).
    public static func relative(_ timestamp: TimestampMs, now: Date = Date()) -> String {
        let date = Date(msSinceEpoch: timestamp)
        let seconds = now.timeIntervalSince(date)
        if seconds < 45 { return "just now" }
        if seconds < 3_600 { return "\(Int((seconds / 60).rounded()))m ago" }
        if seconds < 86_400 { return "\(Int((seconds / 3_600).rounded()))h ago" }
        if seconds < 172_800 { return "yesterday" }
        return dayMonth.string(from: date)
    }

    /// `+268` / `-0`, drawn in `diffAdd` / `diffRemove` with tabular figures (spec 08 §1).
    public static func diff(added: Int64, removed: Int64) -> (added: String, removed: String) {
        ("+\(added)", "-\(removed)")
    }

    // MARK: - Internals

    private static func trimmed(_ value: Double, places: Int) -> String {
        let text = String(format: "%.\(places)f", value)
        guard text.contains(".") else { return text }
        var trimmed = text
        while trimmed.hasSuffix("0") { trimmed.removeLast() }
        if trimmed.hasSuffix(".") { trimmed.removeLast() }
        return trimmed
    }

    private static let grouping: NumberFormatter = {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        return formatter
    }()

    private static let dayMonth: DateFormatter = {
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("d MMM")
        return formatter
    }()
}
