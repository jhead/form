import SwiftUI
import FormCore
import FormDesign

/// Reasoning, above the reply (spec 10 §5). Auto-expanded while it streams, collapsed once
/// the message is done, and shimmering in `color.thinking` — a soft sweep, deliberately
/// unlike the hard-edged text caret (F6.3).
struct ThinkingBlock: View {
    @Environment(\.theme) private var theme

    let text: String
    let effort: ThinkingLevel?
    let isStreaming: Bool

    /// Nil until the user takes a view; after that their choice wins over the auto-behaviour.
    @State private var override: Bool?

    private var isExpanded: Bool { override ?? isStreaming }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.sm) {
            header

            if isExpanded {
                Text(text)
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.thinking)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, theme.metrics.spacing.xl)
                    .transition(.opacity)
            }
        }
        .animation(theme.motion.animation(.normal), value: isExpanded)
        .accessibilityElement(children: .contain)
    }

    private var header: some View {
        Button {
            override = !isExpanded
        } label: {
            HStack(spacing: theme.metrics.spacing.sm) {
                Image(systemName: "chevron.right")
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))

                Text(label)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.thinking)
                    .shimmer(isStreaming)
            }
            .frame(height: theme.metrics.toolRowHeight, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .animation(theme.motion.animation(.fast), value: isExpanded)
    }

    private var label: String {
        let base = isStreaming ? "Thinking" : "Thought"
        guard let effort, effort != .off else { return base }
        return "\(base) · \(effort.displayName)"
    }
}
