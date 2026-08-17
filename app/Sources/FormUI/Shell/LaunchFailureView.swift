import FormDesign
import SwiftUI

/// What the window shows when the core will not start (spec 09: a visible error state, not a
/// crash). The message is the transport's own — it names the data directory or the ABI
/// mismatch, which is the whole diagnostic value.
public struct LaunchFailureView: View {
    @Environment(\.theme) private var theme

    private let message: String
    private let onRetry: () -> Void
    private let onQuit: () -> Void

    public init(
        message: String,
        onRetry: @escaping () -> Void,
        onQuit: @escaping () -> Void
    ) {
        self.message = message
        self.onRetry = onRetry
        self.onQuit = onQuit
    }

    public var body: some View {
        EmptyState(
            systemImage: "exclamationmark.triangle",
            title: "form could not start its core",
            message: message
        ) {
            HStack(spacing: theme.metrics.spacing.md) {
                FormButton("Try Again", kind: .primary, action: onRetry)
                FormButton("Quit", kind: .secondary, action: onQuit)
            }
        }
        .frame(maxWidth: theme.metrics.contentMaxWidth)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .contentBackground()
    }
}

/// The first frame: the core is opening its store and replaying the seed corpus.
public struct LaunchProgressView: View {
    @Environment(\.theme) private var theme

    public init() {}

    public var body: some View {
        VStack(spacing: theme.metrics.spacing.lg) {
            Wordmark(size: theme.typography.display.size)
                .foregroundStyle(theme.color.textPrimary)
            ProgressView()
                .controlSize(.small)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .contentBackground()
        .accessibilityLabel("Starting form")
    }
}

#Preview("Launch states") {
    ThemePreview {
        LaunchProgressView().frame(height: 180)
        LaunchFailureView(
            message: "startupFailed: could not open ~/Library/Application Support/form",
            onRetry: {}, onQuit: {}
        )
        .frame(height: 260)
    }
    .frame(width: 900)
}
