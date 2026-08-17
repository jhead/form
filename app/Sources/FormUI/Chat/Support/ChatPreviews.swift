import SwiftUI
import FormCore
import FormDesign

/// Previews for the transcript's parts.
///
/// Everything they show comes out of `CoreStores.preview` and `TranscriptBuilder` — the same
/// path the running app takes — so a preview cannot drift from the real rendering, and
/// there is no mock transcript written in Swift anywhere in this module (spec 10 §9).
@MainActor
enum ChatPreviewData {
    static let stores = CoreStores.preview(.populated)

    static var items: [TranscriptItem] {
        TranscriptBuilder.items(
            entries: stores.chat.entries,
            toolRuns: stores.chat.toolRuns,
            turns: stores.chat.turns,
            streamingEntryId: stores.chat.streamingEntryId,
            queued: ["and add a test for it"],
            showsFooters: true)
    }

    static var toolCalls: [ToolCallDisplay] {
        for item in items { if case let .tools(_, calls) = item { return calls } }
        return []
    }

    static var footer: TurnFooterModel {
        for item in items { if case let .footer(_, model) = item { return model } }
        return TurnFooterModel(durationMs: nil, totalTokens: 0, stopReason: .stop)
    }

    static var assistant: AssistantMessage? {
        stores.chat.entries.compactMap { $0.message?.asAssistant }.last
    }
}

#Preview("Tool group · footer · error") {
    ThemePreview {
        ToolCallGroup(calls: ChatPreviewData.toolCalls)
        FormDivider()
        TurnFooter(model: ChatPreviewData.footer)
        TurnFooter(
            model: TurnFooterModel(
                durationMs: 211_000, totalTokens: 5_900, stopReason: .aborted))
        FormDivider()
        InlineErrorBlock(
            model: InlineErrorModel(
                raw: "rate_limit_error: request rate exceeded for claude-opus-5, retry after 14s")
        ) {}
    }
    .frame(width: 900)
}

#Preview("Thinking block") {
    ThemePreview {
        ThinkingBlock(
            text: ChatPreviewData.assistant?.thinking ?? "",
            effort: .high,
            isStreaming: true)
        FormDivider()
        ThinkingBlock(
            text: ChatPreviewData.assistant?.thinking ?? "",
            effort: .high,
            isStreaming: false)
    }
    .frame(width: 720)
}

#Preview("Queued message · jump pill") {
    ThemePreview {
        QueuedMessageRow(text: "and add a test for it", columnWidth: 640) {}
        JumpToLatestPill(isVisible: true, isStreaming: false) {}
        JumpToLatestPill(isVisible: true, isStreaming: true) {}
    }
    .frame(width: 720)
}

#Preview("Context ring") {
    ThemePreview {
        HStack(spacing: 24) {
            ContextRingButton(usage: ChatPreviewData.stores.chat.contextUsage)
            ContextRingButton(usage: nil)
        }
    }
}

#Preview("Composer") {
    ComposerPreview()
}

private struct ComposerPreview: View {
    @State private var draft = ""

    var body: some View {
        ThemePreview {
            ComposerView(stores: ChatPreviewData.stores, text: $draft)
        }
        .frame(width: 820)
    }
}

#Preview("Transcript") {
    TranscriptView(stores: ChatPreviewData.stores, effort: .high)
        .theme(.dark)
        .frame(width: 900, height: 620)
}
