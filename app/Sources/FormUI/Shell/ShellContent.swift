import FormDesign
import SwiftUI

/// The seam between the shell and the two surfaces other workstreams own.
///
/// W10 (`ChatView`) and W12 (`HomeView`) land in sibling directories that W9 must not create
/// files in, and neither type exists while this workstream is in flight. Rather than stub
/// their views — which would collide with the real ones — `RootView` is generic over the two
/// content builders, and the app root supplies them:
///
/// ```swift
/// RootView(stores: stores, appState: state,
///          home: { HomeView() },
///          session: { ChatView(sessionId: $0) })
/// ```
///
/// The parameterless `RootView(stores:appState:)` substitutes `PendingSurface`, so the shell
/// builds, runs and is demonstrable before those workstreams land. See the W9 report.
public struct PendingSurface: View {
    @Environment(\.theme) private var theme

    private let title: String
    private let detail: String

    public init(title: String, detail: String) {
        self.title = title
        self.detail = detail
    }

    public var body: some View {
        EmptyState(systemImage: "square.dashed", title: title, message: detail)
            .frame(maxWidth: theme.metrics.contentMaxWidth, maxHeight: .infinity)
            .frame(maxWidth: .infinity)
    }

    static func home() -> PendingSurface {
        PendingSurface(
            title: "Home is not wired up yet",
            detail: "The analytics dashboard (W12) plugs in here. The core is live — the "
                + "sidebar beside this is reading the seeded corpus."
        )
    }

    static func session() -> PendingSurface {
        PendingSurface(
            title: "Chat is not wired up yet",
            detail: "The transcript and composer (W10) plug in below this header."
        )
    }
}
