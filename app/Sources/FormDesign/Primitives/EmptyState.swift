import SwiftUI

/// Designed empty states, not blank space (F11.12). Icon or wordmark, a title, an optional
/// explanatory line, and an optional action.
public struct EmptyState<Action: View>: View {
    @Environment(\.theme) private var theme

    private let systemImage: String?
    private let showsWordmark: Bool
    private let title: String
    private let message: String?
    private let isCompact: Bool
    private let action: Action

    public init(
        systemImage: String? = nil,
        showsWordmark: Bool = false,
        title: String,
        message: String? = nil,
        isCompact: Bool = false,
        @ViewBuilder action: () -> Action
    ) {
        self.systemImage = systemImage
        self.showsWordmark = showsWordmark
        self.title = title
        self.message = message
        self.isCompact = isCompact
        self.action = action()
    }

    public var body: some View {
        VStack(spacing: isCompact ? theme.metrics.spacing.md : theme.metrics.spacing.lg) {
            if showsWordmark {
                Wordmark(size: isCompact ? nil : theme.typography.display.size)
                    .foregroundStyle(theme.color.textPrimary)
            } else if let systemImage {
                Image(systemName: systemImage)
                    .font(.system(size: isCompact ? theme.metrics.iconLarge : 28, weight: .light))
                    .foregroundStyle(theme.color.textTertiary)
            }

            VStack(spacing: theme.metrics.spacing.sm) {
                Text(title)
                    .typeStyle(isCompact ? theme.typography.uiMedium : theme.typography.heading)
                    .foregroundStyle(theme.color.textPrimary)
                    .multilineTextAlignment(.center)

                if let message {
                    Text(message)
                        .typeStyle(isCompact ? theme.typography.micro : theme.typography.caption)
                        .foregroundStyle(theme.color.textTertiary)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            action
        }
        .padding(isCompact ? theme.metrics.spacing.xl : theme.metrics.spacing.xl2)
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
    }
}

public extension EmptyState where Action == EmptyView {
    init(
        systemImage: String? = nil,
        showsWordmark: Bool = false,
        title: String,
        message: String? = nil,
        isCompact: Bool = false
    ) {
        self.init(
            systemImage: systemImage, showsWordmark: showsWordmark, title: title,
            message: message, isCompact: isCompact, action: { EmptyView() }
        )
    }
}

#Preview("EmptyState") {
    ThemePreview {
        EmptyState(
            showsWordmark: true,
            title: "Nothing open",
            message: "Start a chat, or pick a session from the sidebar."
        ) {
            FormButton("New chat", systemImage: "plus", kind: .primary) {}
        }

        FormDivider()

        EmptyState(
            systemImage: "chart.bar",
            title: "No activity in this range",
            message: "Tokens per day will appear here once you've sent a message.",
            isCompact: true
        )
    }
    .frame(width: 720)
}
