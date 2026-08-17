import FormDesign
import SwiftUI

/// The seam between the shell and the two surfaces other workstreams own.
///
/// W10's `ChatView` and W12's dashboard live in sibling directories W9 must not create files
/// in, and they land on their own schedule. Rather than stub their views — which would
/// collide with the real ones — `RootView` is generic over the two content builders and the
/// app root supplies them, so adopting a surface is a one-line change in `FormApp.swift`:
///
/// ```swift
/// RootView(stores:appState:themeController:toasts:,
///          home: { HomeView(stores: stores) },
///          session: { _ in ChatView(stores: stores) })
/// ```
///
/// `ChatView` is wired; `home` is still a `PendingSurface` because W12 exposes no top-level
/// view yet. See the W9 report.
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

    public static func home() -> PendingSurface {
        PendingSurface(
            title: "Home is not wired up yet",
            detail: "The analytics dashboard (W12) plugs in here. The core is live — the "
                + "sidebar beside this is reading the seeded corpus."
        )
    }

    public static func session() -> PendingSurface {
        PendingSurface(
            title: "Chat is not wired up yet",
            detail: "The transcript and composer (W10) plug in below this header."
        )
    }
}
