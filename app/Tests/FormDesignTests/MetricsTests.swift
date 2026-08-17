import Testing

@testable import FormDesign

struct MetricsTests {
    @Test("the spacing ladder matches spec 08 §2.3")
    func spacingLadder() {
        #expect(SpacingScale.standard.all == [2, 4, 6, 8, 12, 16, 20, 24, 32, 40, 48])
    }

    @Test("the spacing ladder is strictly increasing")
    func spacingIsMonotonic() {
        let all = SpacingScale.standard.all
        #expect(zip(all, all.dropFirst()).allSatisfy { $0 < $1 })
    }

    @Test("radii match spec 08 §2.3")
    func radii() {
        let radius = RadiusScale.standard
        #expect(radius.sm == 4)
        #expect(radius.md == 6)
        #expect(radius.lg == 8)
        #expect(radius.xl == 12)
        #expect(radius.pill == 999)
    }

    @Test("named metrics match spec 08 §2.3")
    func namedMetrics() {
        let metrics = MetricTokens.standard
        #expect(metrics.sidebarWidth == 300)
        #expect(metrics.sidebarMinWidth == 220)
        #expect(metrics.sidebarMaxWidth == 420)
        #expect(metrics.sidebarRowHeight == 32)
        #expect(metrics.navRowHeight == 34)
        #expect(metrics.headerHeight == 44)
        #expect(metrics.contentMaxWidth == 720)
        #expect(metrics.composerMaxWidth == 680)
        #expect(metrics.composerMaxLines == 12)
        #expect(metrics.iconButton == 28)
        #expect(metrics.avatar == 24)
    }

    @Test("the sidebar width sits inside its own bounds")
    func sidebarBounds() {
        let metrics = MetricTokens.standard
        #expect(metrics.sidebarMinWidth < metrics.sidebarWidth)
        #expect(metrics.sidebarWidth < metrics.sidebarMaxWidth)
        #expect(metrics.composerMaxWidth <= metrics.contentMaxWidth)
    }

    @Test("both themes share one metric set")
    func metricsAreShared() {
        // Metrics are layout, not palette — a theme that changed them would break the
        // crossfade in F5.4 by relaying out the whole window.
        var light = Theme.light.metrics
        var dark = Theme.dark.metrics
        light.hairline = 0
        dark.hairline = 0
        #expect(light == dark)
    }
}
