import SwiftUI
import FormCore
import FormDesign

/// The composer (spec 10 §6, spec 08 §1): a chip row, W13's attachment tray, the autogrowing
/// field, and a control row carrying the model picker and the context ring.
struct ComposerView: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores
    @Binding var text: String

    /// Every way an attachment can arrive — `+`, drop, paste — goes through W13's intake
    /// (spec 13, Part B).
    ///
    /// Held in `@State` so it lives as long as the composer, and *resolved through*
    /// `ComposerControllers` so the value SwiftUI discards on every rebuild is the same
    /// object rather than a fresh one: the initializer claims `CoreStores.onEvent`, and a
    /// second intake would silently take the sink from the one the tray is bound to.
    @State private var intake: AttachmentIntake

    init(stores: CoreStores, text: Binding<String>) {
        self.stores = stores
        _text = text
        _intake = State(initialValue: ComposerControllers.intake(for: stores))
    }

    private var chat: ChatStore { stores.chat }

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.md) {
            ComposerChipRow(stores: stores)

            AttachmentTray(intake: intake)

            field

            ComposerControlRow(
                stores: stores,
                isStreaming: chat.isStreaming,
                canSend: canSend,
                onSend: send,
                onStop: stop,
                onAttach: intake.presentOpenPanel)
        }
        .frame(maxWidth: theme.metrics.composerMaxWidth)
        .padding(.horizontal, theme.metrics.spacing.xxxl)
        .padding(.bottom, theme.metrics.spacing.xxl)
        // Drop and paste, with the whole composer as the target (F3.1).
        .attachmentDropTarget(intake)
        // `Esc` aborts wherever focus is inside the composer (F1.6).
        .onExitCommand(perform: stop)
        .onChange(of: chat.sessionId, initial: true) { _, id in intake.sessionId = id }
    }

    private var field: some View {
        ZStack(alignment: .bottomTrailing) {
            FormTextEditor(
                text: $text,
                placeholder: placeholder,
                maxLines: theme.metrics.composerMaxLines)
            .overlay(
                RoundedRectangle(cornerRadius: theme.metrics.radius.xl, style: .continuous)
                    .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
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
    }

    /// The placeholder says what `⏎` will actually do. `interrupt` does not replace the
    /// running turn — nothing can — it stops the run and the queued prompt starts the next
    /// one (`defaults.queueMode`, mirrored onto `ChatStore`).
    private var placeholder: String {
        guard chat.isStreaming else { return "Ask anything…" }
        return chat.queueMode == .interrupt ? "Stop and send…" : "Queue a message…"
    }

    // MARK: - Actions

    private func send() {
        let outgoing = text
        guard !outgoing.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let attachmentIds = intake.readyAttachmentIds
        // Clear immediately: the store either dispatches or queues, and both are the user's
        // message leaving the field (F1.7).
        text = ""
        intake.clearAfterSend()
        Task { try? await chat.send(outgoing, attachmentIds: attachmentIds) }
    }

    private func stop() {
        guard chat.isStreaming else { return }
        Task { try? await chat.abort() }
    }
}
