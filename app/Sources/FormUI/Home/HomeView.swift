import FormCore
import FormDesign
import SwiftUI

/// The Home dashboard (F11, spec 12). **Owner: W12.**
///
/// A scrolling, centered column of cards over exactly one `UsageStats` document per period.
/// Nothing here queries the core per chart and nothing aggregates: every number on screen is
/// a field the document already carried (spec 12 §5).
public struct HomeView: View {
    @Environment(\.theme) private var theme

    private let stores: CoreStores
    private let onOpenSession: (String) -> Void

    /// UI state, not user settings — the core's `Settings` has no slot for it, and it is
    /// per-window presentation rather than something to sync (spec 12 §1: both persist).
    @AppStorage("home.tab") private var storedTab = HomeTab.overview.rawValue
    @AppStorage("home.period") private var storedPeriod = StatsRange.d30.rawValue

    /// Set only by previews, which must be able to pin a period the seeded store actually
    /// holds rather than inherit whatever the last launch persisted.
    @State private var pinned: (tab: HomeTab, period: StatsRange)?

    private var metrics: HomeMetrics { .standard }

    public init(stores: CoreStores, onOpenSession: ((String) -> Void)? = nil) {
        self.stores = stores
        self.onOpenSession =
            onOpenSession
            ?? { sessionId in
                Task { await stores.select(sessionId) }
            }
    }

    init(stores: CoreStores, tab: HomeTab, period: StatsRange) {
        self.init(stores: stores)
        _pinned = State(initialValue: (tab, period))
    }

    private var tab: Binding<HomeTab> {
        Binding(
            get: { pinned?.tab ?? HomeTab(rawValue: storedTab) ?? .overview },
            set: { value in
                if pinned != nil {
                    pinned?.tab = value
                } else {
                    storedTab = value.rawValue
                }
            })
    }

    private var period: Binding<StatsRange> {
        Binding(
            get: { pinned?.period ?? StatsRange(rawValue: storedPeriod) ?? .d30 },
            set: { value in
                if pinned != nil {
                    pinned?.period = value
                } else {
                    storedPeriod = value.rawValue
                }
            })
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
                header
                controls
                content
            }
            .frame(maxWidth: theme.metrics.dashboardMaxWidth, alignment: .leading)
            .padding(.horizontal, theme.metrics.spacing.xxxl)
            .padding(.vertical, theme.metrics.spacing.xl2)
            .frame(maxWidth: .infinity)
        }
        .contentBackground()
        .environment(\.statsToken, statsToken)
        .task(id: period.wrappedValue) {
            await stores.stats.select(period.wrappedValue)
        }
    }

    // MARK: Header

    private var header: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.sm) {
            Wordmark()
                .foregroundStyle(theme.color.textSecondary)

            Text(HomeGreeting.current())
                .typeStyle(theme.typography.display)
                .foregroundStyle(theme.color.textPrimary)

            Text(subtitle)
                .typeStyle(theme.typography.caption)
                .foregroundStyle(theme.color.textTertiary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var subtitle: String {
        guard let stats = stores.stats.stats, !stats.isEmpty else {
            return "Your dashboard fills in as you use form."
        }
        return
            "\(StatsFormat.grouped(stats.headline.turns)) turns and \(StatsFormat.abbreviated(stats.headline.totalTokens)) tokens over \(period.wrappedValue.subtitle)."
    }

    private var controls: some View {
        HStack(alignment: .center, spacing: theme.metrics.spacing.xl) {
            SegmentedToggle(
                selection: tab,
                segments: HomeTab.allCases.map {
                    .init(value: $0, title: $0.title, systemImage: $0.systemImage)
                })
            .fixedSize()

            Spacer(minLength: theme.metrics.spacing.lg)

            SegmentedToggle(
                selection: period,
                segments: StatsRange.allCases.map { .init(value: $0, title: $0.segmentTitle) },
                height: theme.metrics.controlHeightMedium
            )
            .fixedSize()
        }
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        if let stats = stores.stats.stats {
            switch tab.wrappedValue {
            case .overview:
                OverviewTab(stats: stats, metrics: metrics)
            case .models:
                ModelsTab(stats: stats, metrics: metrics)
            case .activity:
                ActivityTab(stats: stats, onOpenSession: onOpenSession, metrics: metrics)
            case .cost:
                CostTab(stats: stats, metrics: metrics)
            }
        } else {
            DashboardSkeleton(metrics: metrics)
        }
    }

    /// Changes on every new document, which is what the cards animate against.
    private var statsToken: String {
        guard let stats = stores.stats.stats else { return "loading" }
        return "\(period.wrappedValue.rawValue)-\(stats.generatedAt)"
    }
}

/// The display-scale greeting above the dashboard (spec 12 §1). Serif, by way of
/// `typography.display`.
enum HomeGreeting {
    static func current(_ date: Date = Date()) -> String {
        switch Calendar.current.component(.hour, from: date) {
        case 0 ..< 5: "Still up?"
        case 5 ..< 12: "Good morning"
        case 12 ..< 17: "Good afternoon"
        case 17 ..< 22: "Good evening"
        default: "Good evening"
        }
    }
}

#Preview("Home — overview") {
    HomeView(stores: .preview(.populated), tab: .overview, period: .d30)
        .theme(.light)
        .frame(width: 1_180, height: 900)
}

#Preview("Home — overview, dark") {
    HomeView(stores: .preview(.populated), tab: .overview, period: .d30)
        .theme(.dark)
        .frame(width: 1_180, height: 900)
}

#Preview("Home — models") {
    HomeView(stores: .preview(.populated), tab: .models, period: .all)
        .theme(.dark)
        .frame(width: 1_180, height: 900)
}

#Preview("Home — activity") {
    HomeView(stores: .preview(.populated), tab: .activity, period: .d30)
        .theme(.light)
        .frame(width: 1_180, height: 900)
}

#Preview("Home — cost") {
    HomeView(stores: .preview(.populated), tab: .cost, period: .all)
        .theme(.dark)
        .frame(width: 1_180, height: 900)
}

/// First launch: the store holds a zero document, so every card shows its own designed
/// empty state rather than a blank panel (F11.12).
#Preview("Home — empty") {
    HomeView(stores: .preview(.empty), tab: .overview, period: .d7)
        .theme(.light)
        .frame(width: 1_180, height: 900)
}
