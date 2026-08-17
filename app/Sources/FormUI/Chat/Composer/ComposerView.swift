import AppKit
import SwiftUI
import FormCore
import FormDesign
import UniformTypeIdentifiers

/// The composer (spec 10 §6, spec 08 §1): a chip row, the autogrowing field, and a control
/// row carrying the model picker and the context ring.
struct ComposerView: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    @Binding var text: String

    @State private var isTargetedForDrop = false

    private var chat: ChatStore { stores.chat }

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            ComposerChipRow(stores: stores)

            field

            ComposerControlRow(
                stores: stores,
                isStreaming: chat.isStreaming,
                canSend: canSend,
                onSend: send,
                onStop: stop,
                onAttach: chooseAttachments)
        }
        .frame(maxWidth: theme.metrics.composerMaxWidth)
        .padding(.horizontal, theme.metrics.spacing.xxxl)
        .padding(.bottom, theme.metrics.spacing.xxl)
        .onDrop(of: [.fileURL, .image], isTargeted: $isTargetedForDrop, perform: handleDrop)
        // `Esc` aborts wherever focus is inside the composer (F1.6).
        .onExitCommand(perform: stop)
    }

    private var field: some View {
        ZStack(alignment: .bottomTrailing) {
            FormTextEditor(
                text: $text,
                placeholder: placeholder,
                maxLines: theme.metrics.composerMaxLines)
            .overlay(
                RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                    .strokeBorder(
                        isTargetedForDrop ? theme.color.borderFocus : theme.color.border,
                        lineWidth: theme.metrics.hairline * 2)
            )

            // The `⏎` glyph at the trailing inner edge (spec 08 §1).
            Image(systemName: "return")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(canSend ? theme.color.textSecondary : theme.color.textTertiary)
                .padding(theme.metrics.spacing.lg)
                .allowsHitTesting(false)
        }
        // `⏎` sends, `⇧⏎` and `⌥⏎` insert a newline (F1.8). `TextEditor` has no `onSubmit`,
        // so the key has to be read before it reaches the text system.
        .onKeyPress(phases: .down) { key in
            guard key.key == .return else { return .ignored }
            if key.modifiers.contains(.shift) || key.modifiers.contains(.option) {
                return .ignored
            }
            send()
            return .handled
        }
        .animation(theme.motion.animation(.fast), value: isTargetedForDrop)
    }

    private var placeholder: String {
        chat.isStreaming ? "Queue a message…" : "Ask anything…"
    }

    // MARK: - Actions

    private func send() {
        let outgoing = text
        guard !outgoing.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        // Clear immediately: the store either dispatches or queues, and both are the user's
        // message leaving the field (F1.7).
        text = ""
        Task { try? await chat.send(outgoing) }
    }

    private func stop() {
        guard chat.isStreaming else { return }
        Task { try? await chat.abort() }
    }

    /// The `+` button (F3.1). Both this and the drop path end at the same command, so the
    /// core's registry — hashing, dedupe, size and type rejection — is the only gatekeeper.
    private func chooseAttachments() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.prompt = "Attach"
        guard panel.runModal() == .OK else { return }
        for url in panel.urls { attach(url) }
    }

    private func attach(_ url: URL) {
        guard let sessionId = chat.sessionId else { return }
        let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
            ?? "application/octet-stream"
        Task {
            try? await stores.client.dispatch(
                .addAttachment(
                    sessionId: sessionId, path: url.path, filename: url.lastPathComponent,
                    mime: mime))
        }
    }

    /// Files and images dropped on the composer (F3.1). The tray that shows them is W13's;
    /// the drop target is the composer's.
    private func handleDrop(_ providers: [NSItemProvider]) -> Bool {
        guard chat.sessionId != nil else { return false }
        var accepted = false
        for provider in providers where provider.hasItemConformingToTypeIdentifier(
            UTType.fileURL.identifier)
        {
            accepted = true
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                Task { @MainActor in attach(url) }
            }
        }
        return accepted
    }
}
