import FormDesign
import SwiftUI

/// The dashboard's chart container (spec 12 §3).
///
/// Every chart on every tab goes through here, which is what makes the four tabs agree on
/// card chrome, legend placement, empty treatment and motion. `FormCard` supplies the panel;
/// this adds the parts only a chart needs.
struct ChartCard<Content: View, Accessory: View>: View {
    @Environment(\.theme) private var theme
    @Environment(\.statsToken) private var statsToken

    let title: String
    var subtitle: String?
    var legend: [ChartLegendItem] = []
    /// Plot height. `nil` lets the content size itself (tables, leaderboards).
    var height: CGFloat?
    var isEmpty = false
    var emptyIcon = "chart.bar"
    var emptyTitle = "No data in this range"
    var emptyMessage: String?

    @ViewBuilder var content: Content
    @ViewBuilder var accessory: Accessory

    @State private var hasAppeared = false

    var body: some View {
        FormCard(title: title, subtitle: subtitle) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
                if isEmpty {
                    EmptyState(
                        systemImage: emptyIcon,
                        title: emptyTitle,
                        message: emptyMessage,
                        isCompact: true
                    )
                    .frame(height: height)
                } else {
                    content
                        .frame(height: height)
                    if !legend.isEmpty {
                        ChartLegend(items: legend)
                    }
                }
            }
            // Period change reshapes the marks in place rather than snapping (spec 12 §3);
            // `motion.animation` is nil under reduce-motion, so this costs nothing there.
            .animation(theme.motion.animation(.slow, curve: .emphasized), value: statsToken)
            .opacity(hasAppeared ? 1 : 0)
            .onAppear {
                withAnimation(theme.motion.animation(.slow, curve: .emphasized)) {
                    hasAppeared = true
                }
            }
        } accessory: {
            accessory
        }
    }
}

extension ChartCard where Accessory == EmptyView {
    init(
        title: String,
        subtitle: String? = nil,
        legend: [ChartLegendItem] = [],
        height: CGFloat? = nil,
        isEmpty: Bool = false,
        emptyIcon: String = "chart.bar",
        emptyTitle: String = "No data in this range",
        emptyMessage: String? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title, subtitle: subtitle, legend: legend, height: height, isEmpty: isEmpty,
            emptyIcon: emptyIcon, emptyTitle: emptyTitle, emptyMessage: emptyMessage,
            content: content, accessory: { EmptyView() })
    }
}

/// A card value that cannot be computed yet — fewer than three active days, so percentiles
/// and projections would be noise (spec 12 §4).
struct SparseValue: View {
    @Environment(\.theme) private var theme

    var reason = "Needs at least three active days before this is worth reporting."

    var body: some View {
        Text("—")
            .typeStyle(theme.typography.title)
            .foregroundStyle(theme.color.textTertiary)
            .formTooltip("Not enough data yet", detail: reason)
    }
}

#Preview("ChartCard states") {
    HomePreviewStage {
        VStack(spacing: 16) {
            ChartCard(
                title: "Tokens over time", subtitle: "Last 30 days",
                legend: ChartSeries.tokenSeries.map { ChartLegendItem($0) },
                height: HomeMetrics.standard.chart
            ) {
                TokensOverTimeChart(daily: HomePreviewData.populated.daily)
            }

            ChartCard(
                title: "Cost", isEmpty: true, emptyIcon: "dollarsign.circle",
                emptyTitle: "No spend in this range",
                emptyMessage: "Cost appears once a run reports usage."
            ) {
                EmptyView()
            }

            FormCard(title: "Projected monthly") {
                SparseValue()
            }
        }
    }
}
