import FormDesign
import SwiftUI

/// The loading state (spec 12 §4): skeleton cards with a shimmer, laid out in the same shape
/// the real dashboard will take, so nothing jumps when the document arrives.
struct DashboardSkeleton: View {
    @Environment(\.theme) private var theme

    var metrics: HomeMetrics = .standard

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xl) {
            LazyVGrid(
                columns: [
                    GridItem(.adaptive(minimum: metrics.tileMinWidth), spacing: theme.metrics.spacing.xl)
                ],
                spacing: theme.metrics.spacing.xl
            ) {
                ForEach(0 ..< 8, id: \.self) { _ in
                    FormCard {
                        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
                            ShimmerBlock(width: metrics.rankLabelWidth / 2, height: theme.metrics.spacing.md)
                            ShimmerBlock(width: metrics.rankLabelWidth * 0.6, height: theme.metrics.spacing.xxl)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }

            card(height: metrics.chartCompact)

            HStack(alignment: .top, spacing: theme.metrics.spacing.xl) {
                card(height: metrics.chart)
                card(height: metrics.chart)
            }
        }
        .accessibilityLabel("Loading your dashboard")
    }

    private func card(height: CGFloat) -> some View {
        FormCard {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
                ShimmerBlock(width: metrics.rankLabelWidth, height: theme.metrics.spacing.lg)
                ShimmerBlock(height: height, radius: theme.metrics.radius.lg)
            }
        }
    }
}
