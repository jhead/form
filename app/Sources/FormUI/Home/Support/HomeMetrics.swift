import CoreGraphics

/// The layout numbers spec 12 names that `MetricTokens` does not carry yet — 11 pt heatmap
/// cells with 3 pt gaps, the chart heights, the donut.
///
/// They belong in `FormDesign` beside `dashboardMaxWidth`, and W12 has asked W8 to adopt
/// them. Until then they sit in exactly one struct so the move is mechanical and no view
/// holds a bare number. Everything expressible in the spacing or radius scale reads from
/// `theme.metrics` instead of appearing here.
struct HomeMetrics: Sendable {
    /// GitHub-style contribution cell and the gap between cells (spec 12 §2).
    var heatmapCell: CGFloat = 11
    var heatmapGap: CGFloat = 3
    /// The weekday × hour matrix is read across a 24-column row, so its cells are wider.
    var matrixCell: CGFloat = 13

    /// Plot heights. Three sizes only — a dashboard with five is a dashboard that drifts.
    var chart: CGFloat = 180
    var chartTall: CGFloat = 240
    var chartCompact: CGFloat = 116

    var donut: CGFloat = 168
    var donutThickness: CGFloat = 26

    /// Minimum widths the adaptive grids lay out against.
    var tileMinWidth: CGFloat = 150
    var columnMinWidth: CGFloat = 320
    var tableMinWidth: CGFloat = 620

    /// The hover readout card and its legend swatches.
    var readoutWidth: CGFloat = 200
    var legendSwatch: CGFloat = 9

    /// A ranked bar row: the label gutter, and the bar itself.
    var rankLabelWidth: CGFloat = 116
    var rankBarHeight: CGFloat = 8

    static let standard = HomeMetrics()
}
