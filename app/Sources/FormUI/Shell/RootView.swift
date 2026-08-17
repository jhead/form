import SwiftUI
import FormCore
import FormDesign
import FormMarkdown

/// The app shell. **Owner: W9** — see `docs/specs/09-app-shell-sidebar.md`.
public struct RootView: View {
    public init() {}
    public var body: some View {
        // TODO(W9): NavigationSplitView, sidebar, Home/Code routing, toasts.
        VStack(spacing: 8) {
            Wordmark(size: 32)
            Text("scaffold").font(.footnote).foregroundStyle(.secondary)
        }
        .frame(minWidth: 900, minHeight: 600)
    }
}
