import FormCore
import FormDesign
import Foundation
import SwiftUI

/// Documents the previews render. All three are `UsageStats` values and nothing else — the
/// dashboard is a pure function of one of these, which is what makes a preview a real test
/// of what the app will draw (spec 12 §5).
enum HomePreviewData {
    /// The deterministic 90-day dashboard `MockCorpus` seeds, with no Rust build.
    static let populated: UsageStats = MockCorpus.demo.stats[.d30] ?? UsageStats(range: .d30)

    static let allTime: UsageStats = MockCorpus.demo.stats[.all] ?? UsageStats(range: .all)

    /// First launch: a fully-populated document of zeros (spec 03 §3).
    static let empty = UsageStats(range: .d7)

    /// Under three active days, where percentiles and projections must show `—`
    /// (spec 12 §4). Built by keeping the two most recent active days of the real document
    /// and zeroing the rest, so every array keeps the shape the core would send.
    static let sparse: UsageStats = {
        var stats = populated
        let active = stats.daily.filter { $0.turns > 0 }.suffix(2).map(\.date)
        let keep = Set(active)

        stats.daily = stats.daily.map { bucket in
            guard !keep.contains(bucket.date) else { return bucket }
            return DailyBucket(date: bucket.date)
        }
        stats.heatmap = stats.heatmap.map { cell in
            keep.contains(cell.date) ? cell : HeatmapCell(date: cell.date)
        }
        stats.headline.activeDays = keep.count
        stats.headline.currentStreak = keep.count
        stats.headline.turns = stats.daily.reduce(0) { $0 + $1.turns }
        stats.headline.totalTokens = stats.daily.reduce(0) { $0 + $1.totalTokens }
        stats.cost.projectedMonthly = 0
        return stats
    }()
}

/// The frame every Home preview renders in: one theme, dashboard padding, a scrolling
/// column. Keeps each `#Preview` down to the view under test and the document it is fed.
struct HomePreviewStage<Content: View>: View {
    var theme: Theme = .light
    var width: CGFloat = 900
    @ViewBuilder var content: Content

    var body: some View {
        ScrollView {
            content
                .padding(theme.metrics.spacing.xxxl)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(width: width)
        .background(theme.color.background)
        .theme(theme)
    }
}
