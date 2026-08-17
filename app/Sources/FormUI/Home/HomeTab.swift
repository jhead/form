import FormCore
import FormDesign
import SwiftUI

/// The dashboard's four tabs (F11). The raw value is what persists across launches.
enum HomeTab: String, CaseIterable, Identifiable, Sendable {
    case overview, models, activity, cost

    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .models: "Models"
        case .activity: "Activity"
        case .cost: "Cost"
        }
    }

    var systemImage: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .models: "cpu"
        case .activity: "waveform.path.ecg"
        case .cost: "dollarsign.circle"
        }
    }
}

extension StatsRange {
    /// `7d` / `30d` / `All` — the period selector's labels (spec 12 §1).
    var segmentTitle: String { displayName }

    var subtitle: String {
        switch self {
        case .d7: "the last 7 days"
        case .d30: "the last 30 days"
        case .all: "all time"
        }
    }
}
