import SwiftUI
import FormCore
import FormDesign

/// The transcript: a bottom-anchored scroll of message rows in a 720 pt column, with a
/// jump-to-latest pill when the user has scrolled away (spec 10 §1, §2, spec 08 §1).
struct TranscriptView: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    let effort: ThinkingLevel?

    @State private var scroll = TranscriptScrollState()

    private var chat: ChatStore { stores.chat }

    private static let bottomAnchor = "transcript.bottom"
    private static let scrollSpace = "transcript.scroll"

    private var items: [TranscriptItem] {
        TranscriptBuilder.items(
            entries: chat.entries,
            toolRuns: chat.toolRuns,
            turns: chat.turns,
            streamingEntryId: chat.streamingEntryId,
            queued: chat.queued,
            showsFooters: stores.settings.settings.appearance.showTurnFooters
        )
    }

    var body: some View {
        GeometryReader { outer in
            let columnWidth = min(
                theme.metrics.contentMaxWidth, outer.size.width - theme.metrics.spacing.xxxl * 2)

            ScrollViewReader { reader in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
                        ForEach(items) { item in
                            row(item, columnWidth: columnWidth)
                                .id(item.id)
                        }
                        Color.clear
                            .frame(height: theme.metrics.spacing.xs)
                            .id(Self.bottomAnchor)
                    }
                    .frame(width: columnWidth, alignment: .leading)
                    .padding(.horizontal, theme.metrics.spacing.xxxl)
                    .padding(.vertical, theme.metrics.spacing.xxl)
                    .frame(maxWidth: .infinity)
                    .background(metricsProbe(viewport: outer.size.height))
                }
                .coordinateSpace(name: Self.scrollSpace)
                .onPreferenceChange(TranscriptMetricsKey.self) { [scroll] value in
                    Task { @MainActor in scroll.update(value) }
                }
                .onChange(of: scroll.scrollRequest) { _, _ in
                    // No animation while streaming: a spring chasing a growing document
                    // never settles, and the caret moving is the motion (spec 11 §4).
                    reader.scrollTo(Self.bottomAnchor, anchor: .bottom)
                }
                .onChange(of: chat.entries.count) { _, _ in scroll.contentGrew() }
                .onChange(of: contentSignature) { _, _ in scroll.contentGrew() }
                .onChange(of: chat.sessionId) { _, id in scroll.route(to: id) }
                .task { scroll.route(to: chat.sessionId) }
            }
            .overlay(alignment: .bottom) {
                JumpToLatestPill(
                    isVisible: !scroll.isPinned && scroll.metrics.overflows,
                    isStreaming: chat.isStreaming
                ) {
                    scroll.jumpToLatest()
                }
                .padding(.bottom, theme.metrics.spacing.lg)
            }
        }
    }

    /// Cheap proxy for "the tail grew": the streaming message's length. Watching the whole
    /// transcript would allocate on every delta.
    private var contentSignature: Int {
        chat.streamingMessage.map { $0.text.utf8.count + $0.thinking.utf8.count } ?? 0
    }

    private func metricsProbe(viewport: CGFloat) -> some View {
        GeometryReader { geometry in
            Color.clear.preference(
                key: TranscriptMetricsKey.self,
                value: TranscriptMetrics(
                    contentHeight: geometry.size.height,
                    offset: -geometry.frame(in: .named(Self.scrollSpace)).minY,
                    viewportHeight: viewport
                )
            )
        }
    }

    @ViewBuilder
    private func row(_ item: TranscriptItem, columnWidth: CGFloat) -> some View {
        switch item {
        case let .user(entry, message):
            UserMessageRow(
                entry: entry, message: message, columnWidth: columnWidth,
                onRetry: { retry(entry.id) }, onBranch: { branch(entry.id) })

        case let .assistant(entry, message, isStreaming):
            AssistantMessageRow(
                entry: entry, message: message, isStreaming: isStreaming, effort: effort,
                client: stores.client,
                onRetry: { retry(entry.id) }, onBranch: { branch(entry.id) })

        case let .tools(_, calls):
            ToolCallGroup(calls: calls)

        case let .footer(_, model):
            TurnFooter(model: model)

        case let .error(_, model, entryId):
            InlineErrorBlock(model: model) { retry(entryId) }

        case let .queued(index, text):
            QueuedMessageRow(text: text, columnWidth: columnWidth) {
                chat.removeQueued(at: index)
            }
        }
    }

    private func retry(_ entryId: String) {
        Task { try? await chat.retry(entryId: entryId) }
    }

    private func branch(_ entryId: String) {
        // The core answers with `session_created`, which `SessionStore` selects on; the shell
        // routes from there.
        Task { try? await chat.branch(fromEntryId: entryId) }
    }
}
