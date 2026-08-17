import SwiftUI
import FormCore
import FormDesign

/// The chat surface: transcript above, composer below, and the empty-session state that
/// becomes the transcript on first send (spec 10 §1, §8).
///
/// **Owner: W10** — see `docs/specs/10-chat.md`.
public struct ChatView: View {
    @Environment(\.theme) private var theme

    private let stores: CoreStores

    /// Lives here, above the empty/transcript branch, so the layout change on first send
    /// cannot drop what the user typed (F1.9).
    @State private var draft = ""

    public init(stores: CoreStores) {
        self.stores = stores
    }

    private var chat: ChatStore { stores.chat }
    private var isEmpty: Bool { chat.entries.isEmpty && chat.queued.isEmpty }
    private var effort: ThinkingLevel? { stores.sessions.selected?.modelRef.thinkingLevel }

    public var body: some View {
        VStack(spacing: 0) {
            // Three fixed slots: the composer keeps its position — and so its identity, its
            // focus and its text — across the transition.
            if isEmpty {
                Spacer(minLength: 0)
                greeting
                    .transition(.opacity)
            } else {
                TranscriptView(stores: stores, effort: effort)
                    .transition(.opacity)
            }

            ComposerView(stores: stores, text: $draft)
                .frame(maxWidth: .infinity)

            if isEmpty {
                Spacer(minLength: 0)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.color.background)
        .animation(theme.motion.animation(.slow, curve: .emphasized), value: isEmpty)
    }

    /// Display serif, centred over the composer, in the 680 pt column (spec 08 §1).
    private var greeting: some View {
        VStack(spacing: theme.metrics.spacing.md) {
            Text("What are we building?")
                .typeStyle(theme.typography.display)
                .foregroundStyle(theme.color.textPrimary)

            Text(subtitle)
                .typeStyle(theme.typography.caption)
                .foregroundStyle(theme.color.textTertiary)
        }
        .frame(maxWidth: theme.metrics.composerMaxWidth)
        .padding(.bottom, theme.metrics.spacing.xxl)
        .accessibilityElement(children: .combine)
    }

    private var subtitle: String {
        guard let root = stores.sessions.selected?.workspaceName else {
            return "No workspace folder yet"
        }
        return "in \(root)"
    }
}

// MARK: - Previews

#Preview("Chat — populated") {
    ChatView(stores: .preview(.populated))
        .theme(.light)
        .frame(width: 900, height: 700)
}

#Preview("Chat — populated, dark") {
    ChatView(stores: .preview(.populated))
        .theme(.dark)
        .frame(width: 900, height: 700)
}

#Preview("Chat — empty") {
    ChatView(stores: .preview(.empty))
        .theme(.light)
        .frame(width: 900, height: 700)
}

#Preview("Chat — streaming") {
    StreamingChatPreview()
}

/// Replays a recorded run at real cadence, so deltas, the thinking shimmer, the tool group
/// and the ring all move in the canvas with no Rust build (spec 07 §6).
private struct StreamingChatPreview: View {
    @State private var stores = CoreStores.preview(.populated)

    var body: some View {
        ChatView(stores: stores)
            .theme(.dark)
            .frame(width: 900, height: 700)
            .task { stores.startPreviewStreaming() }
    }
}
