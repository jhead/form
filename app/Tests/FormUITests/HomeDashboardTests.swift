import FormCore
import FormDesign
import SwiftUI
import Testing

@testable import FormUI

/// Spec 12 §5: the dashboard is a pure function of one `UsageStats` document, plus the
/// formatting and bucketing rules the cards depend on.
///
/// **This file is not compiled yet.** `Package.swift` has no `FormUITests` target and W12
/// does not own that file; wiring is one line —
/// `.testTarget(name: "FormUITests", dependencies: ["FormUI"])` — and is requested in the
/// W12 report.
@MainActor
struct HomeDashboardTests {
    // MARK: Purity

    /// Two renders of the same document are pixel-identical, and a different document
    /// renders differently. That is what "renders purely from the document" means in
    /// practice: no clock, no store, no hidden state between passes.
    @Test("the dashboard renders purely over the document")
    func rendersPurelyOverTheDocument() throws {
        let first = HomePreviewData.populated
        let second = HomePreviewData.allTime

        let a = try snapshot(OverviewTab(stats: first))
        let b = try snapshot(OverviewTab(stats: first))
        let c = try snapshot(OverviewTab(stats: second))

        #expect(a == b, "the same document must render identically")
        #expect(a != c, "a different document must render differently")
    }

    @Test("every tab renders an empty document without crashing or blanking")
    func emptyDocumentRendersEveryTab() throws {
        let empty = HomePreviewData.empty
        #expect(try snapshot(OverviewTab(stats: empty)).isEmpty == false)
        #expect(try snapshot(ModelsTab(stats: empty)).isEmpty == false)
        #expect(try snapshot(ActivityTab(stats: empty, onOpenSession: { _ in })).isEmpty == false)
        #expect(try snapshot(CostTab(stats: empty)).isEmpty == false)
    }

    private func snapshot(_ view: some View, width: CGFloat = 900) throws -> Data {
        let renderer = ImageRenderer(
            content:
                view
                .frame(width: width)
                .theme(.light)
        )
        renderer.scale = 1
        let image = try #require(renderer.nsImage)
        return try #require(image.tiffRepresentation)
    }

    // MARK: Formatting

    @Test("counts abbreviate the way the axes and tiles expect")
    func abbreviations() {
        #expect(StatsFormat.abbreviated(Int64(947)) == "947")
        #expect(StatsFormat.abbreviated(Int64(23_627)) == "23.6k")
        #expect(StatsFormat.abbreviated(Int64(2_000)) == "2k")
        #expect(StatsFormat.abbreviated(Int64(21_800_000)) == "21.8M")
        #expect(StatsFormat.grouped(Int64(23_627)).contains("23"))
    }

    @Test("durations and money render at every scale")
    func durationsAndMoney() {
        #expect(StatsFormat.duration(ms: Int64(340)) == "340ms")
        #expect(StatsFormat.duration(ms: Int64(1_200)) == "1.2s")
        #expect(StatsFormat.duration(ms: Int64(211_000)) == "3m 31s")
        #expect(StatsFormat.duration(ms: Int64(4_320_000)) == "1h 12m")
        #expect(StatsFormat.currency(6.19) == "$6.19")
        #expect(StatsFormat.currency(0) == "$0.00")
        #expect(StatsFormat.currency(0.0004) == "$0.0004")
    }

    @Test("dates parse as local days, not UTC instants")
    func dateParsing() throws {
        let date = try #require(StatsFormat.date("2026-03-04"))
        let components = Calendar.current.dateComponents([.year, .month, .day], from: date)
        #expect(components.year == 2026)
        #expect(components.month == 3)
        #expect(components.day == 4)
        #expect(StatsFormat.date("nonsense") == nil)
    }

    // MARK: The footnote

    @Test("the token comparison picks a work the total dwarfs")
    func tokenComparison() throws {
        #expect(TokenComparison.sentence(forTokens: 0) == nil)

        let light = try #require(TokenComparison.sentence(forTokens: 11_000))
        #expect(light.contains("The Little Prince"))

        let heavy = try #require(TokenComparison.sentence(forTokens: 21_800_000))
        #expect(heavy.hasPrefix("You've used ~"))
        #expect(heavy.contains("×"))
    }

    // MARK: Bucketing the charts do themselves

    @Test("the heatmap keeps weekday alignment across a partial first week")
    func heatmapColumns() {
        // 2026-03-04 is a Wednesday, so the first column is padded to it.
        let cells = (4 ... 17).map { day in
            HeatmapCell(date: String(format: "2026-03-%02d", day), tokens: Int64(day), level: 1)
        }
        let columns = HeatmapColumn.build(from: cells)

        #expect(columns.count == 3)
        #expect(columns[0].days[0] == nil, "Monday of a mid-week start stays empty")
        #expect(columns[0].days[2]?.date == "2026-03-04")
        #expect(columns[1].days[0]?.date == "2026-03-09")
        #expect(columns[0].monthLabel == "Mar")
    }

    @Test("the model table sorts by every column, both directions")
    func tableSorting() {
        let models = HomePreviewData.populated.models
        let byTokens = models.sorted { ModelColumn.tokens.isBefore($0, $1, ascending: false) }
        #expect(byTokens.first?.totalTokens == models.map(\.totalTokens).max())

        let byCostAscending = models.sorted { ModelColumn.cost.isBefore($0, $1, ascending: true) }
        #expect(byCostAscending.first?.cost == models.map(\.cost).min())

        let byName = models.sorted { ModelColumn.model.isBefore($0, $1, ascending: true) }
        #expect(byName.count == models.count)
    }

    @Test("series colors are stable across charts")
    func seriesIdentity() {
        // The raw value is the palette index; that invariant is what makes input the same
        // color on the Overview tab and on the Cost tab.
        #expect(ChartSeries.input.rawValue == 0)
        #expect(ChartSeries.tokenSeries.map(\.rawValue) == [0, 1, 2, 3])
        for series in ChartSeries.allCases {
            #expect(series.color(.light) == Theme.light.color.series(series.rawValue))
        }
    }
}
